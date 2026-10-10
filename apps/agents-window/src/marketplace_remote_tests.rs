use super::*;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};

type Routes = Arc<Mutex<BTreeMap<String, (u16, Vec<u8>)>>>;
struct Server {
    origin: String,
    routes: Routes,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Default::default();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (server_routes, server_requests, server_stop) =
            (routes.clone(), requests.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while request.len() < 16_384 && !request.ends_with(b"\r\n\r\n") {
                    if socket.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                let target = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .to_owned();
                server_requests.lock().unwrap().push(target.clone());
                let response = server_routes
                    .lock()
                    .unwrap()
                    .get(&target)
                    .cloned()
                    .unwrap_or((404, b"not found".to_vec()));
                let header = format!(
                    "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.0,
                    response.1.len()
                );
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(&response.1);
            }
        });
        Self {
            origin,
            routes,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn json(&self, path: &str, value: serde_json::Value) {
        self.bytes(path, serde_json::to_vec(&value).unwrap());
    }
    fn bytes(&self, path: &str, bytes: Vec<u8>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.into(), (200, bytes));
    }
    fn http(&self) -> Http {
        Http::new(&self.origin, &self.origin).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn store(root: &Path) -> MarketplaceStore {
    MarketplaceStore::new(root.join("data"), root.join("workspace"))
}
fn skill() -> RemoteSkill {
    RemoteSkill {
        id: "skills-sh:example/skills:demo".into(),
        name: "demo".into(),
        source: "example/skills".into(),
        installs: 23,
    }
}
fn skill_body(version: &str) -> Vec<u8> {
    format!(
        "---\nname: demo\ndescription: Demonstration {version}\n---\nRead references/info.txt.\n"
    )
    .into_bytes()
}
fn file(path: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({"path":path,"mode":"100644","type":"blob","sha":git_blob_hash(bytes),"size":bytes.len()})
}
fn bundle(server: &Server, digit: char) -> String {
    let revision = digit.to_string().repeat(40);
    let tree = "f".repeat(40);
    let main = skill_body(&digit.to_string());
    let reference = format!("reference version {digit}").into_bytes();
    let license = b"Fixture license notice".to_vec();
    server.json(
        "/repos/example/skills/commits/HEAD",
        serde_json::json!({"sha":revision,"commit":{"tree":{"sha":tree}}}),
    );
    server.json(&format!("/repos/example/skills/git/trees/{tree}?recursive=1"), serde_json::json!({"truncated":false,"tree":[
        file("skills/demo/SKILL.md", &main), file("skills/demo/references/info.txt", &reference), file("LICENSE", &license),
        {"path":"unrelated/huge.bin","type":"blob","mode":"100644","sha":"a".repeat(40),"size":MAX_BUNDLE_BYTES*2}
    ]}));
    server.bytes(
        &format!("/example/skills/{revision}/skills/demo/SKILL.md"),
        main,
    );
    server.bytes(
        &format!("/example/skills/{revision}/skills/demo/references/info.txt"),
        reference,
    );
    server.bytes(&format!("/example/skills/{revision}/LICENSE"), license);
    revision
}

#[test]
fn remote_search_validates_metadata_persists_ttl_and_marks_offline_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let server = Server::new();
    server.json("/api/search?q=demo&limit=100", serde_json::json!({"skills":[
        {"id":"example/skills/demo","name":"demo","source":"example/skills","installs":23},
        {"id":"example/skills/demo","name":"demo","source":"example/skills","installs":19},
        {"id":"evil.invalid/demo","name":"demo","source":"https://evil.invalid","installs":100},
        {"id":"example/skills/../outside","name":"../outside","source":"example/skills","installs":100},
        {"id":"wrong/identity","name":"other","source":"example/skills","installs":100}
    ]}));
    let first = store
        .search_remote_from("demo", false, &server.origin)
        .unwrap();
    assert_eq!(first.skills, vec![skill()]);
    assert!(!first.from_cache);
    assert!(!first.stale);
    assert!(
        !store.registry_path().exists(),
        "search must not install or register sources"
    );
    let fresh_store = MarketplaceStore::new(store.data_root.clone(), store.workspace.clone());
    let cached = fresh_store
        .search_remote_from("demo", false, "http://127.0.0.1:1")
        .unwrap();
    assert!(cached.from_cache);
    assert!(!cached.stale);
    assert_eq!(cached.skills, first.skills);
    server.routes.lock().unwrap().clear();
    let fallback = store
        .search_remote_from("demo", true, &server.origin)
        .unwrap();
    assert!(fallback.stale && fallback.from_cache);
    assert!(fallback.warning.unwrap().contains("HTTP 404"));
    assert_eq!(
        store.cached_remote_skill(&skill().id).unwrap(),
        Some(skill())
    );
    assert!(store
        .search_remote_from("missing", true, &server.origin)
        .is_err());
    for query in ["", "a", "ab\ncd"] {
        assert!(store
            .search_remote_from(query, false, &server.origin)
            .is_err());
    }
    assert_eq!(
        server.requests.lock().unwrap().len(),
        3,
        "cache and invalid queries must not make requests"
    );
}

#[test]
fn remote_search_rejects_oversized_response_and_discards_corrupt_cache() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let server = Server::new();
    store.ensure_install_parent(&store.store_root()).unwrap();
    fs::write(store.cache_path(), b"not json").unwrap();
    server.bytes(
        "/api/search?q=demo&limit=100",
        vec![b'x'; SEARCH_BYTES as usize + 1],
    );
    assert!(store
        .search_remote_from("demo", false, &server.origin)
        .unwrap_err()
        .to_string()
        .contains("límite"));
    assert!(!store.registry_path().exists());
}

#[test]
fn explicit_remote_install_is_pinned_bounded_and_keeps_license_and_references() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let server = Server::new();
    let revision = bundle(&server, '1');
    assert!(!store.destination("demo", InstallScope::Workspace).exists());
    store
        .install_remote_from(&skill(), InstallScope::Workspace, &server.http())
        .unwrap();
    let destination = store.destination("demo", InstallScope::Workspace);
    assert_eq!(
        fs::read(destination.join("SKILL.md")).unwrap(),
        skill_body("1")
    );
    assert_eq!(
        fs::read_to_string(destination.join("references/info.txt")).unwrap(),
        "reference version 1"
    );
    assert_eq!(
        fs::read_to_string(destination.join("_upstream/LICENSE")).unwrap(),
        "Fixture license notice"
    );
    let provenance = store.installed_provenance(&skill().id).unwrap().unwrap();
    assert_eq!(provenance.revision, revision);
    assert_eq!(provenance.directory, "skills/demo");
    assert_eq!(provenance.manifest_digest.unwrap().len(), 64);
    let item = store
        .snapshot()
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.id == skill().id)
        .unwrap();
    assert_eq!(item.installed_scope, Some(InstallScope::Workspace));
    assert!(item.enabled && item.source_available);
    assert!(item.license.unwrap().contains("_upstream/"));
    assert!(!server
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|path| path.contains("huge.bin")));
    assert!(store
        .install_remote_from(&skill(), InstallScope::Workspace, &server.http())
        .is_err());
    assert!(store.snapshot().unwrap().sources.is_empty());
}

#[test]
fn remote_update_checks_do_not_install_and_explicit_update_preserves_scope_and_disabled_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let server = Server::new();
    let first = bundle(&server, '1');
    store
        .install_remote_from(&skill(), InstallScope::User, &server.http())
        .unwrap();
    store.set_enabled(&skill().id, false).unwrap();
    let second = bundle(&server, '2');
    let updates = store.check_updates_from(&server.http()).unwrap();
    assert_eq!(
        updates,
        [SkillUpdate {
            item_id: skill().id,
            installed_revision: Some(first),
            available_revision: second.clone()
        }]
    );
    assert_eq!(
        fs::read(
            store
                .destination("demo", InstallScope::User)
                .join("SKILL.md")
        )
        .unwrap(),
        skill_body("1")
    );
    store
        .update_from(&skill().id, &server.http(), |_, _| {
            panic!("community update must not call starter downloader")
        })
        .unwrap();
    let item = store
        .snapshot()
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.id == skill().id)
        .unwrap();
    assert!(!item.enabled);
    assert_eq!(item.installed_scope, Some(InstallScope::User));
    assert_eq!(
        store
            .installed_provenance(&skill().id)
            .unwrap()
            .unwrap()
            .revision,
        second
    );
    assert_eq!(
        fs::read(
            store
                .destination("demo", InstallScope::User)
                .join("SKILL.md")
        )
        .unwrap(),
        skill_body("2")
    );
    assert!(store.check_updates_from(&server.http()).unwrap().is_empty());
}

#[test]
fn bad_blob_and_git_links_never_publish_or_replace_an_existing_installation() {
    for mode in [
        "bad-hash",
        "120000",
        "160000",
        "truncated",
        "path-traversal",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let server = Server::new();
        bundle(&server, '1');
        store
            .install_remote_from(&skill(), InstallScope::User, &server.http())
            .unwrap();
        let registry_before = fs::read(store.registry_path()).unwrap();
        let revision = bundle(&server, '2');
        let tree_path = format!(
            "/repos/example/skills/git/trees/{}?recursive=1",
            "f".repeat(40)
        );
        if mode == "bad-hash" {
            server.bytes(
                &format!("/example/skills/{revision}/skills/demo/references/info.txt"),
                b"wrong content".to_vec(),
            );
        } else {
            let mut routes = server.routes.lock().unwrap();
            let entry = routes.get_mut(&tree_path).unwrap();
            let mut tree: serde_json::Value = serde_json::from_slice(&entry.1).unwrap();
            if mode == "truncated" {
                tree["truncated"] = true.into();
            } else if mode == "path-traversal" {
                tree["tree"][1]["path"] = "skills/demo/../../outside".into();
            } else {
                tree["tree"][1]["mode"] = mode.into();
                if mode == "160000" {
                    tree["tree"][1]["type"] = "commit".into();
                }
            }
            entry.1 = serde_json::to_vec(&tree).unwrap();
        }
        assert!(
            store
                .update_from(&skill().id, &server.http(), |_, _| unreachable!())
                .is_err(),
            "{mode}"
        );
        assert_eq!(
            fs::read(store.registry_path()).unwrap(),
            registry_before,
            "{mode}"
        );
        assert_eq!(
            fs::read(
                store
                    .destination("demo", InstallScope::User)
                    .join("SKILL.md")
            )
            .unwrap(),
            skill_body("1"),
            "{mode}"
        );
        assert!(!temp.path().join("outside").exists());
        assert!(fs::read_dir(
            store
                .destination("demo", InstallScope::User)
                .parent()
                .unwrap()
        )
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')));
    }
}

#[test]
fn failed_registry_commit_rolls_back_the_directory_and_does_not_enable_the_skill() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let server = Server::new();
    bundle(&server, '1');
    store
        .install_remote_from(&skill(), InstallScope::User, &server.http())
        .unwrap();
    store.set_enabled(&skill().id, false).unwrap();
    let registry = store.load_registry().unwrap();
    let mut replacement = registry.installed[0].clone();
    replacement.description = "new".into();
    let old_registry = fs::read(store.registry_path()).unwrap();
    let destination = store.destination("demo", InstallScope::User);
    let staging = destination.with_file_name(".staging-test");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("SKILL.md"), skill_body("2")).unwrap();
    let unwritable_target = temp.path().join("is-a-directory");
    fs::create_dir(&unwritable_target).unwrap();
    let error = store
        .publish_installation_with(registry, replacement, &staging, true, |_| {
            fs::write(&unwritable_target, "registry")
        })
        .unwrap_err();
    assert_ne!(error.kind(), ErrorKind::NotFound);
    assert_eq!(
        fs::read(destination.join("SKILL.md")).unwrap(),
        skill_body("1")
    );
    assert_eq!(fs::read(store.registry_path()).unwrap(), old_registry);
    assert!(!store.load_registry().unwrap().installed[0].enabled);
}

#[test]
fn old_records_load_and_remote_provenance_cannot_override_identity_or_paths() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let old = serde_json::json!({"sources":[],"installed":[{"item_id":"starter-anthropic:frontend-design","name":"frontend-design","description":"old","license":null,"source_name":"old","source_kind":"github","scope":"user","workspace":null,"enabled":true}]});
    store.ensure_install_parent(&store.store_root()).unwrap();
    fs::write(store.registry_path(), serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(store.load_registry().unwrap().installed[0]
        .provenance
        .is_none());
    let server = Server::new();
    bundle(&server, '1');
    let updates = store.check_updates_from(&server.http()).unwrap();
    assert!(updates.iter().any(
        |update| update.item_id == "starter-anthropic:frontend-design"
            && update.installed_revision.is_none()
    ));
    let mut registry = store.load_registry().unwrap();
    let entry = &mut registry.installed[0];
    entry.item_id = skill().id;
    entry.name = "demo".into();
    entry.provenance = Some(SkillProvenance {
        repository: "other/repo".into(),
        revision: "1".repeat(40),
        directory: "skills/demo".into(),
        manifest_digest: None,
    });
    assert!(validate_registry(&registry).is_err());
    registry.installed[0]
        .provenance
        .as_mut()
        .unwrap()
        .repository = "example/skills".into();
    registry.installed[0].provenance.as_mut().unwrap().directory = "../outside".into();
    assert!(validate_registry(&registry).is_err());
    assert!(!store.destination("demo", InstallScope::User).exists());
}

#[test]
fn portable_paths_and_remote_ids_reject_cross_platform_escape_forms() {
    for path in [
        "../x",
        "/x",
        "a//b",
        "a/../b",
        "a\\b",
        "C:/x",
        "a/NUL.txt",
        "a/file.",
        "a/file ",
    ] {
        assert!(!safe_relative(path), "{path}");
    }
    for path in [
        "SKILL.md",
        ".agents/skills/demo/SKILL.md",
        "references/guía.txt",
    ] {
        assert!(safe_relative(path), "{path}");
    }
    for id in [
        "skills-sh:https://evil.test:demo",
        "skills-sh:example/repo:../outside",
        "skills-sh:example/repo/demo:demo",
        "skills-sh:../repo:demo",
    ] {
        assert!(remote_identity(id).is_none(), "{id}");
    }
    assert_eq!(
        git_blob_hash(b"test\n"),
        "9daeafb9864cf43055ae93beb0afd6c7d144bfa4"
    );
}

#[test]
fn expired_search_cache_is_refreshed_and_ancient_results_are_not_presented_as_current() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let server = Server::new();
    let mut cache = SearchCache {
        entries: vec![CachedSearch {
            query: "demo".into(),
            fetched_at: unix_now() - CACHE_TTL - 1,
            skills: vec![skill()],
        }],
    };
    store.write_search_cache(&cache).unwrap();
    let stale = store
        .search_remote_from("demo", false, &server.origin)
        .unwrap();
    assert!(stale.stale && stale.from_cache && stale.warning.is_some());
    cache.entries[0].fetched_at = unix_now() - CACHE_MAX_AGE - 1;
    store.write_search_cache(&cache).unwrap();
    assert!(store
        .search_remote_from("demo", false, &server.origin)
        .is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[test]
fn curated_updates_compare_manifest_provenance_and_preserve_disabled_workspace_install() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(temp.path());
    let id = "starter-anthropic:frontend-design";
    let download = |_: &catalog::StarterEntry, destination: &Path| {
        fs::create_dir(destination)?;
        fs::write(
            destination.join("SKILL.md"),
            "---\nname: frontend-design\ndescription: fixture\n---\nfirst",
        )
    };
    store
        .install_with_downloader(id, InstallScope::Workspace, download)
        .unwrap();
    store.set_enabled(id, false).unwrap();
    let mut registry = store.load_registry().unwrap();
    registry.installed[0]
        .provenance
        .as_mut()
        .unwrap()
        .manifest_digest = Some("0".repeat(64));
    store.save_registry(&registry).unwrap();
    let server = Server::new();
    assert_eq!(
        store.check_updates_from(&server.http()).unwrap().len(),
        1,
        "same commit with a different reviewed file manifest must still be an update"
    );
    assert!(
        server.requests.lock().unwrap().is_empty(),
        "curated revision checks are offline"
    );
    store
        .update_from(id, &server.http(), |_, destination| {
            fs::create_dir(destination)?;
            fs::write(
                destination.join("SKILL.md"),
                "---\nname: frontend-design\ndescription: fixture\n---\nsecond",
            )
        })
        .unwrap();
    assert!(store.check_updates_from(&server.http()).unwrap().is_empty());
    let entry = &store.load_registry().unwrap().installed[0];
    assert!(!entry.enabled);
    assert_eq!(entry.scope, InstallScope::Workspace);
    assert_eq!(entry.provenance, starter_provenance(id));
    assert!(fs::read_to_string(
        store
            .destination("frontend-design", InstallScope::Workspace)
            .join("SKILL.md")
    )
    .unwrap()
    .ends_with("second"));
}
