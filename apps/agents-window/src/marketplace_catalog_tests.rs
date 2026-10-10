use super::super::{
    active_skill_directories, active_skill_instructions, InstallScope, MarketplaceStore,
};
use super::*;
use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    sync::mpsc,
    thread,
};

fn fixture() -> (StarterEntry, Vec<Vec<u8>>) {
    let bodies = vec![
        b"---\nname: frontend-design\ndescription:\n  A portable fixture that references\n  the complete guide.\nlicense: Apache-2.0\n---\nRead AGENTS.md relative to this skill.\n".to_vec(),
        b"# Reference\nUse the captured local fixture only.\n".to_vec(),
    ];
    let mut entry = entry("starter-anthropic:frontend-design").unwrap().clone();
    entry.files = ["SKILL.md", "AGENTS.md"]
        .into_iter()
        .zip(&bodies)
        .map(|(path, bytes)| StarterFile {
            path: path.into(),
            repository_path: format!("{}/{path}", entry.directory),
            bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        })
        .collect();
    (entry, bodies)
}

fn server(
    responses: Vec<(u16, Vec<u8>, String)>,
) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (sender, requests) = mpsc::channel();
    let worker = thread::spawn(move || {
        for (status, bytes, headers) in responses {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "expected pinned download request"
                        );
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                let end = line == "\r\n";
                request.push_str(&line);
                assert!(request.len() < 16_384);
                if end {
                    break;
                }
            }
            let _ = sender.send(request);
            let _ = write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n", bytes.len());
            let _ = socket.write_all(&bytes);
        }
    });
    (origin, requests, worker)
}

#[test]
fn starter_metadata_is_offline_pinned_small_and_does_not_modify_user_sources() {
    assert!((30..=50).contains(&STARTERS.len()));
    for legacy in [
        "starter-anthropic:frontend-design",
        "starter-vercel:vercel-react-best-practices",
        "starter-vercel:vercel-composition-patterns",
    ] {
        assert!(entry(legacy).is_some(), "stable installed skill identity");
    }
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("not-created");
    let store = MarketplaceStore::new(data.clone(), temp.path().into());
    let snapshot = store.snapshot().unwrap();
    assert!(
        !data.exists(),
        "browsing a fresh catalog creates or downloads nothing"
    );
    assert!(snapshot.sources.is_empty());
    assert_eq!(snapshot.items.len(), STARTERS.len());
    assert!(snapshot
        .items
        .iter()
        .all(|item| item.source_available && !item.enabled && item.installed_scope.is_none()));
    assert!(active_skill_instructions(&data, temp.path())
        .unwrap()
        .is_empty());
    assert!(active_skill_directories(&data, temp.path())
        .unwrap()
        .is_empty());
    for entry in STARTERS.iter() {
        validate_entry(entry).unwrap();
        let details = starter_details(&entry.id).unwrap();
        assert_eq!(details.revision.len(), 40);
        assert!(details.download_bytes < 300_000);
        assert!(!details.category.is_empty());
        assert!(!details.compatibility.is_empty());
        if entry.repository == "anthropics/skills" {
            assert!(entry.files.iter().any(|file| file.path == "LICENSE.txt"));
        } else if entry.repository == "vercel-labs/agent-skills" {
            assert!(entry
                .files
                .iter()
                .any(|file| file.path == "_upstream/README.md"
                    && file.repository_path == "README.md"));
        } else {
            assert!(entry
                .files
                .iter()
                .any(|file| file.path == "_upstream/LICENSE" && file.repository_path == "LICENSE"));
        }
    }
    let publishers: HashSet<_> = STARTERS.iter().map(|entry| &entry.repository).collect();
    let categories: HashSet<_> = STARTERS.iter().map(|entry| &entry.category).collect();
    assert_eq!(publishers.len(), 6);
    assert_eq!(categories.len(), 6);
    assert!(starter_details("starter-vercel:../../escape").is_none());
}

#[test]
fn curated_sources_categories_and_bundle_digests_keep_provenance_exact() {
    let (original, _) = fixture();
    let digest = manifest_digest(&original);
    let mut reordered = original.clone();
    reordered.files.reverse();
    assert_eq!(manifest_digest(&reordered), digest);
    reordered.files.pop();
    assert_ne!(
        manifest_digest(&reordered),
        digest,
        "membership affects updates at the same commit"
    );
    let mut changed = original.clone();
    changed.files[0].sha256 = "a".repeat(64);
    assert_ne!(manifest_digest(&changed), digest);
    changed = original.clone();
    changed.repository = "vercel-labs/agent-skills".into();
    assert!(
        validate_entry(&changed).is_err(),
        "publisher and repository must be an exact pair"
    );
    assert!(is_starter_source("starter-microsoft"));
    assert!(!is_starter_source("starter-microsoft-attacker"));
    assert!(!is_starter_source("starter-microsoft:azure-identity-rust"));
    assert!(starter_manifest_digest("missing").is_none());
    let mut json: serde_json::Value =
        serde_json::from_str(include_str!("../assets/marketplace/starter-catalog.json")).unwrap();
    json[0].as_object_mut().unwrap().remove("category");
    let old: StarterEntry = serde_json::from_value(json[0].clone()).unwrap();
    assert_eq!(old.category, "General");
    validate_entry(&old).unwrap();

    for (path, repository_path) in [
        ("_upstream/source.rs", "src/source.rs"),
        ("_upstream/LICENSE", "../LICENSE"),
        ("_upstream/LICENSE", "other/LICENSE"),
    ] {
        let mut changed = original.clone();
        changed.files[1].path = path.into();
        changed.files[1].repository_path = repository_path.into();
        assert!(
            validate_entry(&changed).is_err(),
            "{path} -> {repository_path}"
        );
    }
}

#[test]
fn pinned_download_installs_only_selected_files_and_preserves_provenance_and_references() {
    let (entry, bodies) = fixture();
    let (origin, requests, worker) = server(
        bodies
            .iter()
            .map(|body| (200, body.clone(), String::new()))
            .collect(),
    );
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("download");
    download_from(&entry, &directory, &origin, Duration::from_secs(3)).unwrap();
    for (file, body) in entry.files.iter().zip(&bodies) {
        assert_eq!(fs::read(directory.join(&file.path)).unwrap(), *body);
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.starts_with(&format!(
            "GET /{}/{}/{} ",
            entry.repository, entry.revision, file.repository_path
        )));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
    }
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join(".openide-source.json")).unwrap()).unwrap();
    assert_eq!(provenance["revision"], entry.revision);
    assert_eq!(provenance["repository"], entry.repository);
    assert_eq!(provenance["manifest_digest"], manifest_digest(&entry));
    worker.join().unwrap();
}

#[test]
fn complete_script_and_license_resources_are_preserved_without_execution() {
    let (mut entry, mut bodies) = fixture();
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("executed");
    let script = format!("#!/bin/sh\nprintf executed > '{}'\n", marker.display()).into_bytes();
    for (path, repository_path, bytes) in [
        (
            "scripts/probe.sh",
            format!("{}/scripts/probe.sh", entry.directory),
            script,
        ),
        (
            "_upstream/LICENSE",
            "LICENSE".into(),
            b"Apache License fixture\n".to_vec(),
        ),
    ] {
        entry.files.push(StarterFile {
            path: path.into(),
            repository_path,
            bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        });
        bodies.push(bytes);
    }
    let (origin, _, worker) = server(
        bodies
            .iter()
            .map(|body| (200, body.clone(), String::new()))
            .collect(),
    );
    let directory = temp.path().join("complete");
    download_from(&entry, &directory, &origin, Duration::from_secs(3)).unwrap();
    worker.join().unwrap();
    assert_eq!(
        fs::read(directory.join("scripts/probe.sh")).unwrap(),
        bodies[2]
    );
    assert_eq!(
        fs::read(directory.join("_upstream/LICENSE")).unwrap(),
        bodies[3]
    );
    assert!(!marker.exists(), "downloading a script never executes it");
}

#[test]
fn starter_install_activation_scoping_restart_disable_uninstall_and_failed_download_are_real() {
    let (fixture, bodies) = fixture();
    let (origin, _, worker) = server(
        bodies
            .iter()
            .map(|body| (200, body.clone(), String::new()))
            .collect(),
    );
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("project");
    fs::create_dir(&workspace).unwrap();
    let store = MarketplaceStore::new(temp.path().join("data"), workspace.clone());
    let local = temp.path().join("personal-skill");
    fs::create_dir(&local).unwrap();
    fs::write(
        local.join("SKILL.md"),
        "---\nname: personal-skill\ndescription: Preserve a personal source\n---\nLocal reference.",
    )
    .unwrap();
    store.add_local_source(&local).unwrap();
    let original_sources = store.snapshot().unwrap().sources;
    store
        .install_with_downloader(&fixture.id, InstallScope::Workspace, |selected, staging| {
            assert_eq!(selected.id, fixture.id);
            download_from(&fixture, staging, &origin, Duration::from_secs(3))
        })
        .unwrap();
    worker.join().unwrap();
    let reopened = MarketplaceStore::new(store.data_root.clone(), workspace.clone());
    let snapshot = reopened.snapshot().unwrap();
    assert_eq!(
        snapshot.sources, original_sources,
        "curated install does not replace or add user source records"
    );
    let item = snapshot
        .items
        .iter()
        .find(|item| item.id == fixture.id)
        .unwrap();
    assert_eq!(item.installed_scope, Some(InstallScope::Workspace));
    assert!(item.enabled);
    let roots = active_skill_directories(&store.data_root, &workspace).unwrap();
    assert_eq!(
        roots,
        [store
            .destination(&fixture.name, InstallScope::Workspace)
            .canonicalize()
            .unwrap()]
    );
    let instructions = active_skill_instructions(&store.data_root, &workspace).unwrap();
    assert!(instructions.contains(&serde_json::to_string(&roots[0].to_string_lossy()).unwrap()));
    assert!(instructions.contains("\"name\":\"frontend-design\""));
    assert!(instructions.contains("A portable fixture that references the complete guide."));
    assert!(
        !instructions.contains("Read AGENTS.md relative to this skill."),
        "skill bodies are loaded on demand, not injected with the index"
    );
    assert!(roots[0].join("AGENTS.md").is_file());
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    assert!(active_skill_directories(&store.data_root, &other)
        .unwrap()
        .is_empty());
    reopened.set_enabled(&fixture.id, false).unwrap();
    assert!(active_skill_instructions(&store.data_root, &workspace)
        .unwrap()
        .is_empty());
    assert!(active_skill_directories(&store.data_root, &workspace)
        .unwrap()
        .is_empty());
    reopened.uninstall(&fixture.id).unwrap();
    assert!(!roots[0].exists());
    assert!(reopened
        .snapshot()
        .unwrap()
        .items
        .iter()
        .any(|item| item.id == fixture.id
            && item.source_available
            && item.installed_scope.is_none()));

    let before = fs::read(store.registry_path()).unwrap();
    let failure = store
        .install_with_downloader(&fixture.id, InstallScope::User, |_, staging| {
            fs::create_dir(staging)?;
            fs::write(staging.join("partial"), "incomplete")?;
            Err(io::Error::other("fixture offline"))
        })
        .unwrap_err();
    assert!(failure.to_string().contains("fixture offline"));
    assert_eq!(fs::read(store.registry_path()).unwrap(), before);
    assert!(!store
        .destination(&fixture.name, InstallScope::User)
        .exists());
    assert!(fs::read_dir(
        store
            .destination(&fixture.name, InstallScope::User)
            .parent()
            .unwrap()
    )
    .unwrap()
    .next()
    .is_none());
}

#[test]
fn integrity_limits_redirects_and_path_escape_fail_closed() {
    let (entry, bodies) = fixture();
    for mode in ["hash", "size", "redirect", "http-error"] {
        let temp = tempfile::tempdir().unwrap();
        let (status, body, headers) = match mode {
            "hash" => {
                let mut body = bodies[0].clone();
                body[0] = b'x';
                (200, body, String::new())
            }
            "size" => (200, vec![0; bodies[0].len() + 1], String::new()),
            "redirect" => (
                302,
                vec![],
                "Location: http://127.0.0.1:1/private\r\n".into(),
            ),
            _ => (
                503,
                b"untrusted response not surfaced".to_vec(),
                String::new(),
            ),
        };
        let (origin, _, worker) = server(vec![(status, body, headers)]);
        let error = download_from(
            &entry,
            &temp.path().join("download"),
            &origin,
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(!error
            .to_string()
            .contains("untrusted response not surfaced"));
        assert!(!temp.path().join("download/SKILL.md").exists());
        worker.join().unwrap();
    }
    for path in ["../escape", "/absolute", "x\\escape", "x//y", "C:escape"] {
        let mut invalid_entry = entry.clone();
        invalid_entry.files[0].path = path.into();
        assert!(validate_entry(&invalid_entry).is_err(), "{path}");
    }
    let mut excessive = entry.clone();
    excessive.files[0].bytes = MAX_BUNDLE_BYTES + 1;
    assert!(validate_entry(&excessive).is_err());
    let temp = tempfile::tempdir().unwrap();
    let error = download_from(
        &entry,
        &temp.path().join("deadline"),
        "http://127.0.0.1:1",
        Duration::ZERO,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[cfg(unix)]
#[test]
fn starter_install_does_not_follow_destination_aliases_or_download_on_name_collision() {
    let temp = tempfile::tempdir().unwrap();
    let store = MarketplaceStore::new(temp.path().join("data"), temp.path().into());
    let destination = store.destination("frontend-design", InstallScope::User);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, &destination).unwrap();
    let mut requested = false;
    assert!(store
        .install_with_downloader(
            "starter-anthropic:frontend-design",
            InstallScope::User,
            |_, _| {
                requested = true;
                Ok(())
            }
        )
        .is_err());
    assert!(!requested);
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    fs::remove_file(&destination).unwrap();
    fs::remove_dir(destination.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, destination.parent().unwrap()).unwrap();
    assert!(store
        .install_with_downloader(
            "starter-anthropic:frontend-design",
            InstallScope::User,
            |_, _| {
                requested = true;
                Ok(())
            }
        )
        .is_err());
    assert!(!requested);
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
}

/// Explicit integration check against the immutable upstream commits. It
/// never reads credentials or writes to the user's marketplace/workspace.
#[test]
#[ignore = "Downloads all curated pinned skills from official raw.githubusercontent.com into temporary data"]
fn install_curated_catalog_from_official_sources_smoke() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("isolated-workspace");
    fs::create_dir(&workspace).unwrap();
    let data = temporary.path().join("isolated-data");
    let store = MarketplaceStore::new(data.clone(), workspace.clone());
    let started = Instant::now();
    let mut downloaded_files = 0;
    let mut downloaded_bytes = 0;
    assert_eq!(store.snapshot().unwrap().items.len(), STARTERS.len());
    assert!(
        !data.exists(),
        "offline metadata must not initialize an installation"
    );
    for entry in STARTERS.iter() {
        let start = Instant::now();
        store
            .install(&entry.id, InstallScope::Workspace)
            .unwrap_or_else(|error| {
                panic!("{} failed after {:?}: {error}", entry.id, start.elapsed())
            });
        let directory = store.destination(&entry.name, InstallScope::Workspace);
        let body = fs::read_to_string(directory.join("SKILL.md")).unwrap();
        assert_eq!(
            super::super::parse_frontmatter(&body).unwrap().0,
            entry.name
        );
        for file in &entry.files {
            let actual = fs::read(directory.join(&file.path)).unwrap();
            assert_eq!(
                actual.len() as u64,
                file.bytes,
                "{} / {}",
                entry.id,
                file.path
            );
            assert_eq!(
                format!("{:x}", Sha256::digest(&actual)),
                file.sha256,
                "{} / {}",
                entry.id,
                file.path
            );
            downloaded_files += 1;
            downloaded_bytes += actual.len();
        }
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join(".openide-source.json")).unwrap())
                .unwrap();
        assert_eq!(provenance["item_id"], entry.id);
        assert_eq!(provenance["revision"], entry.revision);
        assert_eq!(provenance["repository"], entry.repository);
        assert_eq!(provenance["manifest_digest"], manifest_digest(entry));
        if entry.repository == "vercel-labs/agent-skills" {
            assert!(directory.join("AGENTS.md").is_file());
            assert!(directory.join("rules").is_dir() || directory.join("references").is_dir());
            assert!(fs::read_to_string(directory.join("_upstream/README.md"))
                .unwrap()
                .contains("MIT"));
        } else if entry.repository == "anthropics/skills" {
            assert!(fs::read_to_string(directory.join("LICENSE.txt"))
                .unwrap()
                .contains("Apache License"));
        } else {
            let license = fs::read_to_string(directory.join("_upstream/LICENSE")).unwrap();
            assert!(license.contains(if entry.license == "MIT" {
                "MIT License"
            } else {
                "Apache License"
            }));
        }
        println!(
            "{}: {} verified files, {} bytes, {:.2}s",
            entry.id,
            entry.files.len(),
            entry.files.iter().map(|file| file.bytes).sum::<u64>(),
            start.elapsed().as_secs_f64()
        );
    }
    let reopened = MarketplaceStore::new(data.clone(), workspace.clone());
    let snapshot = reopened.snapshot().unwrap();
    assert!(snapshot.sources.is_empty());
    assert_eq!(
        snapshot
            .items
            .iter()
            .filter(|item| item.installed_scope == Some(InstallScope::Workspace) && item.enabled)
            .count(),
        STARTERS.len()
    );
    assert_eq!(
        active_skill_directories(&data, &workspace).unwrap().len(),
        STARTERS.len()
    );
    let active = active_skill_instructions(&data, &workspace).unwrap();
    for entry in STARTERS.iter() {
        assert!(active.contains(&format!("\"name\":\"{}\"", entry.name)));
        reopened.set_enabled(&entry.id, false).unwrap();
        assert!(!active_skill_instructions(&data, &workspace)
            .unwrap()
            .contains(&format!("\"name\":\"{}\"", entry.name)));
        assert!(!active_skill_directories(&data, &workspace)
            .unwrap()
            .contains(
                &reopened
                    .destination(&entry.name, InstallScope::Workspace)
                    .canonicalize()
                    .unwrap()
            ));
        reopened.set_enabled(&entry.id, true).unwrap();
    }
    assert_eq!(
        active_skill_directories(&data, &workspace).unwrap().len(),
        STARTERS.len()
    );
    println!("Curated catalog: {downloaded_files} files, {downloaded_bytes} bytes, {:.2}s total; temporary installations removed on exit", started.elapsed().as_secs_f64());
}
