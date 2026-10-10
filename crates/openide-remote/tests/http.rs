use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    routing::post,
    Json, Router,
};
use http_body_util::BodyExt;
use openide_core::{AgentEvent, AgentEventKind, SessionStore, TurnMode};
use openide_remote::{
    api,
    auth::DeviceStore,
    config::{Config, Project, ProviderConfig},
    host::{CreateSession, Host, StartTurn},
    projection, storage,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    project: PathBuf,
    config: Config,
    host: Arc<Host>,
    app: Router,
    token: String,
    device_id: String,
    calls: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.shutdown();
        self.server.abort();
    }
}

impl Fixture {
    async fn new(write_tool: bool) -> Self {
        Self::with_write_permission(write_tool, true).await
    }

    async fn with_write_permission(write_tool: bool, allow_write: bool) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Router::new().route("/v1/chat/completions", post(move |State(calls): State<Arc<AtomicUsize>>, Json(body): Json<Value>| async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(body["model"], "fixture");
            let message = if n == 0 && write_tool {
                json!({"role":"assistant","content":"I will write the receipt.","tool_calls":[{"id":format!("call-receipt-{}",body["messages"].as_array().unwrap().len()),"type":"function","function":{"name":"write_file","arguments":"{\"path\":\"receipt.txt\",\"content\":\"approved exactly once\"}"}}]})
            } else { json!({"role":"assistant","content":"Task complete."}) };
            Json(json!({"id":format!("response-{n}"),"choices":[{"index":0,"message":message,"finish_reason":if n == 0 && write_tool { "tool_calls" } else { "stop" }}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}))
        })).with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, provider).await.unwrap();
        });
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("state");
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(
            project.join("AGENTS.md"),
            "PRIVATE_PROJECT_INSTRUCTION_SENTINEL: keep changes small.",
        )
        .unwrap();
        let config = Config {
            version: 1,
            bind: "127.0.0.1:0".parse().unwrap(),
            provider: ProviderConfig {
                id: "fixture".into(),
                protocol: "chat_completions".into(),
                endpoint,
                model: "fixture".into(),
                api_key_env: None,
                desktop_data_root: None,
                tool_calls: true,
                image_input: false,
            },
            projects: vec![Project {
                id: "project".into(),
                name: "Test project".into(),
                path: project.clone(),
            }],
            allow_write,
            allow_commands: false,
            command_isolation: openide_harness::CommandIsolation::HostUnrestricted,
        };
        let model = openide_remote::config::build_backend(&config.provider).unwrap();
        let host = Host::open(&root, config.clone(), model).unwrap();
        let (device, token) = DeviceStore::new(&root).issue("Test iPhone").unwrap();
        let app = api::router(host.clone());
        Self {
            _temp: temp,
            root,
            project,
            config,
            host,
            app,
            token,
            device_id: device.id,
            calls,
            server,
        }
    }

    async fn request(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        request(self.app.clone(), Some(&self.token), method, path, body).await
    }

    async fn session(&self) -> String {
        let (status, value) = self
            .request(
                "POST",
                "/v1/sessions",
                Some(json!({"project_id":"project","request_id":"create-1","title":"Remote test"})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value["id"].as_str().unwrap().to_owned()
    }

    async fn wait(&self, session: &str, expected: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (code, value) = self
                    .request("GET", &format!("/v1/sessions/{session}"), None)
                    .await;
                assert_eq!(code, StatusCode::OK, "{value}");
                if value["status"] == expected {
                    break value;
                }
                if value["status"] == "failed" {
                    let (_, events) = self
                        .request("GET", &format!("/v1/sessions/{session}/events"), None)
                        .await;
                    panic!("unexpected fixture failure: {value}; events {events}");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("run did not reach expected state")
    }
}

#[tokio::test]
async fn per_turn_approval_is_durable_and_cannot_relax_plan_or_host_tool_restrictions() {
    for policy in ["auto", "full_access"] {
        let f = Fixture::new(true).await;
        let session = f.session().await;
        let path = format!("/v1/sessions/{session}/turns");
        let turn = json!({"request_id":"policy-turn","prompt":"Write receipt","mode":"build","approval_policy":policy});
        let (status, accepted) = f.request("POST", &path, Some(turn.clone())).await;
        assert_eq!(status, StatusCode::OK, "{accepted}");
        f.wait(&session, "completed").await;
        assert_eq!(
            std::fs::read_to_string(f.project.join("receipt.txt")).unwrap(),
            "approved exactly once"
        );
        let (_, events) = f
            .request("GET", &format!("/v1/sessions/{session}/events"), None)
            .await;
        assert!(!events.to_string().contains("permission_requested"));
        let (status, replayed) = f.request("POST", &path, Some(turn.clone())).await;
        assert_eq!(status, StatusCode::OK, "{replayed}");
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
        let mut changed = turn;
        changed["approval_policy"] = json!("ask");
        let (status, error) = f.request("POST", &path, Some(changed)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "idempotency_conflict");
        // Selection applies to one admission only. Reset the mock's script to
        // request the write again; an omitted next-turn policy must ask.
        f.calls.store(0, Ordering::SeqCst);
        assert_eq!(
            f.request(
                "POST",
                &path,
                Some(json!({"request_id":"next-default","prompt":"Write again","mode":"build"}))
            )
            .await
            .0,
            StatusCode::OK
        );
        f.wait(&session, "waiting_permission").await;
    }
    for (mode, allow_write) in [("plan", true), ("build", false)] {
        let f = Fixture::with_write_permission(true, allow_write).await;
        let session = f.session().await;
        let (status,response)=f.request("POST",&format!("/v1/sessions/{session}/turns"),Some(json!({"request_id":"restricted","prompt":"Write receipt","mode":mode,"approval_policy":"full_access"}))).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        f.wait(&session, "completed").await;
        assert!(!f.project.join("receipt.txt").exists());
        assert!(f
            .host
            .session(&session)
            .unwrap()
            .pending_permissions
            .is_empty());
    }
}

async fn request(
    app: Router,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .oneshot(
            builder
                .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)}));
    (status, value)
}

#[tokio::test]
async fn attachments_are_device_scoped_and_turn_retry_uses_frozen_admission() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let f = Fixture::new(false).await;
    let session = f.session().await;
    let content = format!("ATTACHMENT_PRIVATE_SENTINEL\n{}", "a".repeat(90 * 1024));
    let upload = json!({"request_id":"upload-text-1","filename":"notes.txt","media_type":"text/plain",
        "data_base64":base64::engine::general_purpose::STANDARD.encode(&content),
        "sha256":format!("{:x}", Sha256::digest(content.as_bytes()))});
    let path = "/v1/projects/project/attachments";
    assert_eq!(
        request(f.app.clone(), None, "POST", path, Some(upload.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (code, attachment) = f.request("POST", path, Some(upload.clone())).await;
    assert_eq!(code, StatusCode::OK, "{attachment}");
    assert_eq!(
        f.request("POST", path, Some(upload.clone())).await.1,
        attachment
    );
    let mut changed_upload = upload;
    changed_upload["filename"] = json!("renamed.txt");
    assert_eq!(
        f.request("POST", path, Some(changed_upload)).await.0,
        StatusCode::CONFLICT
    );
    let turn = json!({"request_id":"attached-turn-1","prompt":"Review these notes","mode":"plan","attachment_ids":[attachment["id"]]});
    let turns_path = format!("/v1/sessions/{session}/turns");
    let (_, other_token) = DeviceStore::new(&f.root).issue("Other iPhone").unwrap();
    assert_eq!(
        request(
            f.app.clone(),
            Some(&other_token),
            "POST",
            &turns_path,
            Some(turn.clone())
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    let (code, accepted) = f.request("POST", &turns_path, Some(turn.clone())).await;
    assert_eq!(code, StatusCode::OK, "{accepted}");
    f.wait(&session, "completed").await;
    let (_, events) = f
        .request("GET", &format!("/v1/sessions/{session}/events"), None)
        .await;
    assert!(events.to_string().contains("ATTACHMENT_PRIVATE_SENTINEL"));
    std::fs::remove_dir_all(f.root.join("attachments")).unwrap();
    let (code, replayed) = f.request("POST", &turns_path, Some(turn.clone())).await;
    assert_eq!(code, StatusCode::OK, "{replayed}");
    assert_eq!(replayed["turn_id"], accepted["turn_id"]);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    let mut changed_turn = turn;
    changed_turn["attachment_ids"] = json!([]);
    let (code, error) = f.request("POST", &turns_path, Some(changed_turn)).await;
    assert_eq!(code, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "idempotency_conflict");
}

#[tokio::test]
async fn v2_workspace_routes_require_auth_and_paths_are_confined() {
    let f = Fixture::new(false).await;
    for (method, path) in [
        ("GET", "/v1/projects/project/files"),
        ("GET", "/v1/projects/project/file?path=x"),
        ("PUT", "/v1/projects/project/file"),
        ("GET", "/v1/projects/project/git/status"),
        ("POST", "/v1/projects/project/git/stage"),
        ("POST", "/v1/projects/project/git/commit"),
        ("PATCH", "/v1/sessions/x"),
        ("GET", "/v1/models"),
    ] {
        assert_eq!(
            request(f.app.clone(), None, method, path, Some(json!({})))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    std::fs::create_dir(f.project.join("src")).unwrap();
    std::fs::write(f.project.join("src/main.rs"), "hello").unwrap();
    std::fs::write(f.project.join("binary"), [0, 255, 1]).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&f.root, f.project.join("outside")).unwrap();
    let (code, tree) = f
        .request("GET", "/v1/projects/project/files?query=main", None)
        .await;
    assert_eq!(code, StatusCode::OK, "{tree}");
    assert_eq!(tree["entries"][0]["path"], "src/main.rs");
    for path in [
        "..%2Fstate%2Fdevices.json",
        "%2Fetc%2Fpasswd",
        "outside%2Fdevices.json",
        "binary",
        ".git%2Fconfig",
    ] {
        assert!(!f
            .request(
                "GET",
                &format!("/v1/projects/project/file?path={path}"),
                None
            )
            .await
            .0
            .is_success());
    }
    assert_eq!(
        f.request("GET", "/v1/projects/unlisted/files", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn editor_cas_serializes_competing_writes_and_replays_the_original_ack() {
    let f = Fixture::new(false).await;
    std::fs::write(f.project.join("edit.txt"), "before").unwrap();
    let (_, original) = f
        .request("GET", "/v1/projects/project/file?path=edit.txt", None)
        .await;
    let payload = |id, content| json!({"request_id":id,"path":"edit.txt","content":content,"expected_revision":original["revision"]});
    let left = payload("write-left", "left");
    let right = payload("write-right", "right");
    let (a, b) = tokio::join!(
        f.request("PUT", "/v1/projects/project/file", Some(left.clone())),
        f.request("PUT", "/v1/projects/project/file", Some(right.clone()))
    );
    let (winning, answer, rejected) = if a.0.is_success() {
        (left, a, b)
    } else {
        (right, b, a)
    };
    assert_eq!(answer.0, StatusCode::OK, "{answer:?}");
    assert_eq!(rejected.0, StatusCode::CONFLICT, "{rejected:?}");
    assert_eq!(rejected.1["error"]["code"], "file_conflict");
    std::fs::write(f.project.join("edit.txt"), "external edit after ACK").unwrap();
    let retry = f
        .request("PUT", "/v1/projects/project/file", Some(winning.clone()))
        .await;
    assert_eq!(retry, answer);
    assert_eq!(
        std::fs::read_to_string(f.project.join("edit.txt")).unwrap(),
        "external edit after ACK"
    );
    let mut changed = winning;
    changed["content"] = json!("different retry");
    assert_eq!(
        f.request("PUT", "/v1/projects/project/file", Some(changed))
            .await
            .1["error"]["code"],
        "idempotency_conflict"
    );
    let large = "x".repeat(100_000);
    let (code, result) = f.request("PUT","/v1/projects/project/file",Some(json!({"request_id":"large-new","path":"large.txt","content":large,"expected_revision":null}))).await;
    assert_eq!(code, StatusCode::OK, "{result}");
    assert_eq!(
        std::fs::metadata(f.project.join("large.txt"))
            .unwrap()
            .len(),
        100_000
    );
}

fn fixture_git(root: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn git_status_stage_unstage_commit_are_real_cas_and_durable_idempotent() {
    let f = Fixture::new(false).await;
    fixture_git(&f.project, &["init", "-q"]);
    fixture_git(&f.project, &["config", "user.name", "Fixture"]);
    fixture_git(
        &f.project,
        &["config", "user.email", "fixture@example.invalid"],
    );
    std::fs::write(f.project.join("new.txt"), "first\n").unwrap();
    let (code, initial) = f
        .request("GET", "/v1/projects/project/git/status", None)
        .await;
    assert_eq!(code, StatusCode::OK, "{initial}");
    assert!(initial["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "new.txt" && f["untracked"] == true));
    let (_, diff) = f
        .request("GET", "/v1/projects/project/git/diff?path=new.txt", None)
        .await;
    assert!(diff["diff"].as_str().unwrap().contains("+first"));
    std::fs::write(f.project.join("new.txt"), "second\n").unwrap();
    let stale = json!({"request_id":"stale-stage","paths":["new.txt"],"expected_revision":initial["revision"]});
    assert_eq!(
        f.request("POST", "/v1/projects/project/git/stage", Some(stale))
            .await
            .1["error"]["code"],
        "git_conflict"
    );
    let (_, current) = f
        .request("GET", "/v1/projects/project/git/status", None)
        .await;
    let stage = json!({"request_id":"stage-one","paths":["new.txt"],"expected_revision":current["revision"]});
    let other = json!({"request_id":"stage-two","paths":["AGENTS.md"],"expected_revision":current["revision"]});
    let (a, b) = tokio::join!(
        f.request(
            "POST",
            "/v1/projects/project/git/stage",
            Some(stage.clone())
        ),
        f.request(
            "POST",
            "/v1/projects/project/git/stage",
            Some(other.clone())
        )
    );
    let (payload, winner, loser) = if a.0.is_success() {
        (stage, a, b)
    } else {
        (other, b, a)
    };
    assert_eq!(winner.0, StatusCode::OK, "{winner:?}");
    assert_eq!(loser.1["error"]["code"], "git_conflict", "{loser:?}");
    assert_eq!(
        f.request(
            "POST",
            "/v1/projects/project/git/stage",
            Some(payload.clone())
        )
        .await,
        winner
    );
    let staged_path = payload["paths"][0].as_str().unwrap();
    let unstage = json!({"request_id":"unstage-one","paths":[staged_path],"expected_revision":winner.1["revision"]});
    let (code, unstaged) = f
        .request("POST", "/v1/projects/project/git/unstage", Some(unstage))
        .await;
    assert_eq!(code, StatusCode::OK, "{unstaged}");
    assert!(fixture_git(&f.project, &["diff", "--cached", "--name-only"]).is_empty());
    let stage = json!({"request_id":"stage-final","paths":["new.txt"],"expected_revision":unstaged["revision"]});
    let (code, staged) = f
        .request("POST", "/v1/projects/project/git/stage", Some(stage))
        .await;
    assert_eq!(code, StatusCode::OK, "{staged}");
    let commit = json!({"request_id":"commit-once","message":"First remote commit","expected_revision":staged["revision"]});
    let result = f
        .request(
            "POST",
            "/v1/projects/project/git/commit",
            Some(commit.clone()),
        )
        .await;
    assert_eq!(result.0, StatusCode::OK, "{result:?}");
    assert_eq!(
        fixture_git(&f.project, &["rev-list", "--count", "HEAD"]),
        "1"
    );
    assert_eq!(fixture_git(&f.project, &["show", "HEAD:new.txt"]), "second");
    assert_eq!(
        f.request("POST", "/v1/projects/project/git/commit", Some(commit))
            .await,
        result
    );
    assert_eq!(
        fixture_git(&f.project, &["rev-list", "--count", "HEAD"]),
        "1"
    );
    assert!(!f.project.join(".git/index.lock").exists());
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn metadata_search_archive_and_pagination_preserve_unique_sessions() {
    let f = Fixture::new(false).await;
    let first = f.session().await;
    let (_, second) = f
        .request(
            "POST",
            "/v1/sessions",
            Some(json!({"project_id":"project","request_id":"create-2","title":"Second task"})),
        )
        .await;
    let patch = json!({"request_id":"rename-1","title":"Find me","archived":true,"favorite":true});
    let (code, updated) = f
        .request(
            "PATCH",
            &format!("/v1/sessions/{first}"),
            Some(patch.clone()),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{updated}");
    assert_eq!(updated["title"], "Find me");
    assert_eq!(updated["archived"], true);
    assert_eq!(updated["favorite"], true);
    assert_eq!(
        f.request("PATCH", &format!("/v1/sessions/{first}"), Some(patch))
            .await
            .0,
        StatusCode::OK
    );
    let (_, searched) = f
        .request("GET", "/v1/sessions?query=Find&archived=true", None)
        .await;
    assert_eq!(searched["sessions"].as_array().unwrap().len(), 1);
    let (_, active) = f.request("GET", "/v1/sessions?archived=false", None).await;
    assert_eq!(active["sessions"][0]["id"], second["id"]);
    let (_, page1) = f.request("GET", "/v1/sessions?limit=1", None).await;
    let cursor = page1["next_cursor"].as_str().unwrap();
    let (_, page2) = f
        .request(
            "GET",
            &format!("/v1/sessions?limit=1&cursor={cursor}"),
            None,
        )
        .await;
    assert_ne!(page1["sessions"][0]["id"], page2["sessions"][0]["id"]);
    assert!(page2["next_cursor"].is_null());
    let (_, models) = f.request("GET", "/v1/models", None).await;
    assert_eq!(models["models"].as_array().unwrap().len(), 1);
    assert!(!models.to_string().contains("endpoint"));
}

#[tokio::test]
async fn read_only_host_rejects_workspace_mutations_and_pairing_ack_is_device_scoped() {
    let f = Fixture::new(false).await;
    let mut config = f.config.clone();
    config.allow_write = false;
    let read_root = f.root.join("read-only-host");
    let model = openide_remote::config::build_backend(&config.provider).unwrap();
    let host = Host::open(&read_root, config, model).unwrap();
    let (_, token) = DeviceStore::new(&read_root)
        .issue("Read-only device")
        .unwrap();
    let app = api::router(host.clone());
    let (_, health) = request(app.clone(), Some(&token), "GET", "/v1/health", None).await;
    assert!(!health["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "workspace.files.write"));
    for (path, body) in [
        (
            "/v1/projects/project/file",
            json!({"request_id":"no-write","path":"blocked.txt","content":"no","expected_revision":null}),
        ),
        (
            "/v1/projects/project/git/stage",
            json!({"request_id":"no-stage","paths":["x"],"expected_revision":"x"}),
        ),
        (
            "/v1/projects/project/git/commit",
            json!({"request_id":"no-commit","message":"x","expected_revision":"x"}),
        ),
    ] {
        let method = if path.ends_with("/file") {
            "PUT"
        } else {
            "POST"
        };
        assert_eq!(
            request(app.clone(), Some(&token), method, path, Some(body))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert!(!f.project.join("blocked.txt").exists());
    let pairs = openide_remote::pairing::PairingStore::new(&f.root);
    let invite = pairs
        .start("https://eas.example.invalid", "My host")
        .unwrap();
    let body = json!({"pairing_id":invite.pairing_id,"secret":invite.secret,"request_id":uuid::Uuid::new_v4().to_string(),"device_name":"New iPhone"});
    let (code, paired) = request(
        f.app.clone(),
        None,
        "POST",
        "/v1/pairing/complete",
        Some(body.clone()),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{paired}");
    assert_eq!(
        request(
            f.app.clone(),
            None,
            "POST",
            "/v1/pairing/complete",
            Some(body)
        )
        .await
        .1,
        paired
    );
    let ack_path = format!("/v1/pairing/{}/ack", invite.pairing_id);
    assert_eq!(
        request(f.app.clone(), None, "POST", &ack_path, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(!f.request("POST", &ack_path, None).await.0.is_success());
    let pair_token = paired["token"].as_str().unwrap();
    assert_eq!(
        request(f.app.clone(), Some(pair_token), "POST", &ack_path, None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(f.app.clone(), Some(pair_token), "GET", "/v1/health", None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(f.request("POST","/v1/notifications/devices",Some(json!({"apns_token":"a".repeat(64),"environment":"production","bundle_id":"dev.openade.remote"}))).await.0,StatusCode::BAD_REQUEST);
    host.shutdown();
}

#[tokio::test]
async fn existing_git_lock_rejects_without_touching_index_or_reexecuting_later() {
    let f = Fixture::new(false).await;
    fixture_git(&f.project, &["init", "-q"]);
    std::fs::write(f.project.join("lock.txt"), "local").unwrap();
    let (_, status) = f
        .request("GET", "/v1/projects/project/git/status", None)
        .await;
    std::fs::write(f.project.join(".git/index.lock"), "external owner").unwrap();
    let body = json!({"request_id":"blocked-by-owner","paths":["lock.txt"],"expected_revision":status["revision"]});
    let result = f
        .request("POST", "/v1/projects/project/git/stage", Some(body.clone()))
        .await;
    assert_eq!(result.1["error"]["code"], "git_conflict", "{result:?}");
    assert_eq!(
        std::fs::read_to_string(f.project.join(".git/index.lock")).unwrap(),
        "external owner"
    );
    std::fs::remove_file(f.project.join(".git/index.lock")).unwrap();
    assert_eq!(
        f.request("POST", "/v1/projects/project/git/stage", Some(body))
            .await,
        result
    );
    assert!(!f.project.join(".git/index").exists());
}

#[tokio::test]
async fn every_route_requires_a_revocable_device_and_tokens_are_not_stored() {
    let f = Fixture::new(false).await;
    for path in ["/v1/health", "/v1/projects", "/v1/sessions", "/v1/unknown"] {
        assert_eq!(
            request(f.app.clone(), None, "GET", path, None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    let bytes = std::fs::read_to_string(f.root.join("devices.json")).unwrap();
    assert!(!bytes.contains(&f.token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f.root.join("devices.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(f.request("GET", "/v1/health", None).await.0, StatusCode::OK);
    DeviceStore::new(&f.root).revoke(&f.device_id).unwrap();
    assert_eq!(
        f.request("GET", "/v1/health", None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn real_http_model_write_requires_durable_approval_and_turn_retries_do_not_repeat() {
    let f = Fixture::new(true).await;
    let session = f.session().await;
    assert_eq!(f.session().await, session);
    let payload = json!({"request_id":"turn-1","prompt":"Write a receipt","mode":"build"});
    let url = format!("/v1/sessions/{session}/turns");
    assert_eq!(
        f.request("POST", &url, Some(payload.clone())).await.0,
        StatusCode::OK
    );
    assert_eq!(
        f.request("POST", &url, Some(payload.clone())).await.0,
        StatusCode::OK
    );
    let pending = f.wait(&session, "waiting_permission").await;
    assert!(!f.project.join("receipt.txt").exists());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    let permission_id = pending["pending_permissions"][0]["request_id"]
        .as_str()
        .unwrap();
    let permission_url = format!("/v1/sessions/{session}/permissions/{permission_id}");
    assert_eq!(
        f.request("POST", &permission_url, Some(json!({"decision":"allow"})))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        f.request("POST", &permission_url, Some(json!({"decision":"allow"})))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        f.request("POST", &permission_url, Some(json!({"decision":"deny"})))
            .await
            .0,
        StatusCode::CONFLICT
    );
    f.wait(&session, "completed").await;
    assert_eq!(
        std::fs::read_to_string(f.project.join("receipt.txt")).unwrap(),
        "approved exactly once"
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        f.request("POST", &url, Some(payload)).await.0,
        StatusCode::OK
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    let changed = json!({"request_id":"turn-1","prompt":"Different input","mode":"build"});
    assert_eq!(
        f.request("POST", &url, Some(changed)).await.0,
        StatusCode::CONFLICT
    );
    let (_, page) = f
        .request(
            "GET",
            &format!("/v1/sessions/{session}/events?after=0&limit=200"),
            None,
        )
        .await;
    let serialized = page.to_string();
    assert!(!serialized.contains("PRIVATE_PROJECT_INSTRUCTION_SENTINEL"));
    assert!(!serialized.contains("system_instructions_committed"));
    assert!(serialized.contains("permission_requested"));
    assert!(serialized.contains("tool_finished"));
    assert!(serialized.contains("turn_completed"));
    let messages: Vec<_> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "assistant_message")
        .collect();
    assert_eq!(messages.len(), 2);
    assert_ne!(messages[0]["message_id"], messages[1]["message_id"]);
}

#[tokio::test]
async fn disconnected_mobile_does_not_stop_a_run_and_paged_replay_is_complete() {
    let f = Fixture::new(false).await;
    let session = f.session().await;
    assert_eq!(
        f.request(
            "POST",
            &format!("/v1/sessions/{session}/turns"),
            Some(json!({"request_id":"background","prompt":"Finish while offline","mode":"plan"}))
        )
        .await
        .0,
        StatusCode::OK
    );
    // No requests while the daemon drains and persists the response.
    tokio::time::sleep(Duration::from_millis(200)).await;
    f.wait(&session, "completed").await;
    let mut cursor = 0;
    let mut seen = Vec::new();
    loop {
        let (_, page) = f
            .request(
                "GET",
                &format!("/v1/sessions/{session}/events?after={cursor}&limit=1"),
                None,
            )
            .await;
        let next = page["next_cursor"].as_u64().unwrap();
        assert!(next >= cursor);
        for event in page["events"].as_array().unwrap() {
            let seq = event["seq"].as_u64().unwrap();
            assert!(seq > cursor);
            seen.push(seq);
        }
        cursor = next;
        if page["has_more"] == false {
            break;
        }
    }
    assert!(!seen.is_empty());
    let store = SessionStore::open(f.root.join("sessions")).unwrap();
    let record = store.load_strict(&session).unwrap().unwrap();
    assert_eq!(cursor, record.events.last().unwrap().sequence);
    let reopened = projection::project_events(&record.events, 0, 200);
    assert_eq!(
        seen,
        reopened.events.iter().map(|e| e.seq).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn interrupted_admission_is_visible_but_never_reexecuted_by_a_retry() {
    let f = Fixture::new(false).await;
    let session = f.session().await;
    // Simulate the durable boundary after admission and before worker startup.
    let store = SessionStore::open(f.root.join("sessions")).unwrap();
    store
        .admit_native_turn(&session, "cut-turn", "Do work", "fixture", TurnMode::Build)
        .unwrap();
    let (_, view) = f
        .request("GET", &format!("/v1/sessions/{session}"), None)
        .await;
    assert_eq!(view["status"], "interrupted");
    let (status, retry) = f
        .request(
            "POST",
            &format!("/v1/sessions/{session}/turns"),
            Some(json!({"request_id":"cut-turn","prompt":"Do work","mode":"build"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry["status"], "interrupted");
    assert_eq!(
        f.request(
            "POST",
            &format!("/v1/sessions/{session}/turns"),
            Some(json!({"request_id":"new-turn","prompt":"Do work again","mode":"build"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_requires_the_matching_turn_and_never_authorizes_a_write() {
    let f = Fixture::new(true).await;
    let session = f.session().await;
    f.request(
        "POST",
        &format!("/v1/sessions/{session}/turns"),
        Some(json!({"request_id":"active-turn","prompt":"Write something","mode":"build"})),
    )
    .await;
    f.wait(&session, "waiting_permission").await;
    let url = format!("/v1/sessions/{session}/cancel");
    assert_eq!(
        f.request(
            "POST",
            &url,
            Some(json!({"request_id":"cancel-1","turn_id":"wrong-turn"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let payload = json!({"request_id":"cancel-1","turn_id":"active-turn"});
    assert_eq!(
        f.request("POST", &url, Some(payload.clone())).await.0,
        StatusCode::OK
    );
    assert_eq!(
        f.request("POST", &url, Some(payload)).await.0,
        StatusCode::OK
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!f.project.join("receipt.txt").exists());
}

#[tokio::test]
async fn unauthorized_projects_and_shell_endpoints_are_unavailable() {
    let f = Fixture::new(false).await;
    assert_eq!(
        f.request(
            "POST",
            "/v1/sessions",
            Some(json!({"project_id":"outside","request_id":"bad-project"}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request("POST", "/v1/terminal/input", Some(json!({"command":"id"})))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request("POST", "/v1/exec", Some(json!({"command":"id"})))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request("GET", "/v1/projects/outside/diff", None).await.0,
        StatusCode::NOT_FOUND
    );
    let another_model = openide_remote::config::build_backend(&f.config.provider).unwrap();
    assert!(Host::open(&f.root, f.config.clone(), another_model).is_err());
}

#[test]
fn private_events_advance_cursor_without_leaking_content() {
    let events = vec![AgentEvent::new(
        "s",
        1,
        AgentEventKind::SystemInstructionsCommitted {
            version: 1,
            digest: "private".into(),
            content: "PRIVATE".into(),
        },
    )];
    let page = projection::project_events(&events, 0, 10);
    assert!(page.events.is_empty());
    assert_eq!(page.next_cursor, 1);
    assert!(!page.has_more);
    assert!(!serde_json::to_string(&page).unwrap().contains("PRIVATE"));
}

#[test]
fn known_subscription_limit_is_explained_without_exposing_provider_errors() {
    let cases = [
        ("model request failed: Responses stream failed: subscription_sharing_usage_limit_exceeded: PRIVATE_PROVIDER_SENTINEL", true),
        ("model request failed: HTTP 401 echoed token PRIVATE_PROVIDER_SENTINEL", false),
        ("PRIVATE_PROVIDER_SENTINEL mentions subscription_sharing_usage_limit_exceeded: in an unrelated error body", false),
    ];
    for (error, quota) in cases {
        let event = AgentEvent::new(
            "session",
            1,
            AgentEventKind::TurnFailed {
                error: error.into(),
            },
        );
        let page = projection::project_events(&[event], 0, 10);
        assert_eq!(page.events[0].kind, "turn_failed");
        let public = page.events[0].text.as_deref().unwrap();
        assert!(!public.contains("PRIVATE_PROVIDER_SENTINEL"));
        assert!(!public.contains("subscription_sharing_usage_limit_exceeded"));
        if quota {
            assert_eq!(public, "Se alcanzó el límite de uso de tu plan de ChatGPT para esta conexión. Reintentá cuando se restablezca la cuota.");
        } else {
            assert_eq!(public, "La ejecución falló. Revisa la conexión, configuración y registro privado del host.");
        }
    }
}

#[test]
fn saved_desktop_credentials_cannot_be_routed_to_an_arbitrary_endpoint() {
    let temp = TempDir::new().unwrap();
    let config = ProviderConfig {
        id: "codex".into(),
        protocol: "responses".into(),
        endpoint: "https://attacker.invalid/v1".into(),
        model: "fixture".into(),
        api_key_env: None,
        desktop_data_root: Some(temp.path().to_owned()),
        tool_calls: true,
        image_input: false,
    };
    assert!(openide_remote::config::build_backend(&config).is_err());
}

#[test]
fn pairing_export_does_not_change_existing_parent_directory_permissions() {
    let temp = TempDir::new().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        storage::write_json(
            &temp.path().join("pairing.json"),
            &json!({"token":"synthetic"}),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

#[test]
fn public_page_byte_budget_does_not_skip_the_first_undelivered_event() {
    // JSON escapes inflate control characters by six; budget serialized bytes.
    let events: Vec<_> = (1..=4)
        .map(|seq| {
            AgentEvent::new(
                "session",
                seq,
                AgentEventKind::ToolFinished {
                    tool_call_id: format!("call-{seq}"),
                    output: "\u{1}".repeat(64 * 1024),
                    is_error: false,
                },
            )
        })
        .collect();
    let first = projection::project_events(&events, 0, 200);
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.next_cursor, 1);
    assert!(first.has_more);
    assert!(serde_json::to_vec(&first).unwrap().len() <= 512 * 1024);
    let next = projection::project_events(&events, first.next_cursor, 200);
    assert_eq!(next.events[0].seq, 2);
}

#[cfg(unix)]
#[test]
fn private_files_reject_symlinks_and_exports_never_replace_files() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("secret.json");
    storage::write_json(&target, &json!({"private":"synthetic"})).unwrap();
    let link = temp.path().join("alias.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(storage::read_private(&link, 1024).is_err());
    assert!(storage::write_new_json(&link, &json!({"replacement":true})).is_err());
    assert!(storage::write_new_json(&target, &json!({"replacement":true})).is_err());
    assert!(
        String::from_utf8(storage::read_private(&target, 1024).unwrap())
            .unwrap()
            .contains("synthetic")
    );
}

#[tokio::test]
async fn reopening_the_host_preserves_completed_history_and_allows_a_new_turn() {
    let f = Fixture::new(false).await;
    let root = f.root.with_file_name("reopened-state");
    let backend = || openide_remote::config::build_backend(&f.config.provider).unwrap();
    let host = Host::open(&root, f.config.clone(), backend()).unwrap();
    let session = host
        .create_session(CreateSession {
            project_id: "project".into(),
            request_id: "new-session".into(),
            title: Some("Persistent".into()),
        })
        .unwrap();
    host.start_turn(
        &session.id,
        StartTurn {
            request_id: "first".into(),
            prompt: "Remember this first message".into(),
            mode: TurnMode::Plan,
            attachment_ids: vec![],
            model_id: None,
            reasoning_effort: None,
            approval_policy: None,
        },
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while host.session(&session.id).unwrap().status != "completed" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let before = serde_json::to_value(host.events(&session.id, 0, 200).unwrap()).unwrap();
    drop(host);
    tokio::time::sleep(Duration::from_millis(60)).await;
    let reopened = Host::open(&root, f.config.clone(), backend()).unwrap();
    assert_eq!(reopened.session(&session.id).unwrap().status, "completed");
    assert_eq!(
        serde_json::to_value(reopened.events(&session.id, 0, 200).unwrap()).unwrap(),
        before
    );
    // A previous command's retry is an ACK, including after reopening.
    let retry = reopened
        .start_turn(
            &session.id,
            StartTurn {
                request_id: "first".into(),
                prompt: "Remember this first message".into(),
                mode: TurnMode::Plan,
                attachment_ids: vec![],
                model_id: None,
                reasoning_effort: None,
                approval_policy: None,
            },
        )
        .unwrap();
    assert_eq!(retry["status"], "completed");
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    reopened
        .start_turn(
            &session.id,
            StartTurn {
                request_id: "second".into(),
                prompt: "Continue the conversation".into(),
                mode: TurnMode::Plan,
                attachment_ids: vec![],
                model_id: None,
                reasoning_effort: None,
                approval_policy: None,
            },
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reopened.session(&session.id).unwrap().status != "completed" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn foreign_or_denied_permissions_never_release_a_write() {
    let f = Fixture::new(true).await;
    let session = f.session().await;
    f.request(
        "POST",
        &format!("/v1/sessions/{session}/turns"),
        Some(json!({"request_id":"denied-turn","prompt":"Write something","mode":"build"})),
    )
    .await;
    let view = f.wait(&session, "waiting_permission").await;
    assert_eq!(
        f.request(
            "POST",
            &format!("/v1/sessions/{session}/permissions/foreign-id"),
            Some(json!({"decision":"allow"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let request_id = view["pending_permissions"][0]["request_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        f.request(
            "POST",
            &format!("/v1/sessions/{session}/permissions/{request_id}"),
            Some(json!({"decision":"deny"}))
        )
        .await
        .0,
        StatusCode::OK
    );
    f.wait(&session, "completed").await;
    assert!(!f.project.join("receipt.txt").exists());
}

#[tokio::test]
async fn actual_cli_validates_config_and_provisions_then_revokes_a_private_token() {
    let f = Fixture::new(false).await;
    let root = f.root.with_file_name("cli-state");
    storage::prepare_directory(&root).unwrap();
    storage::write_json(&root.join("config.json"), &f.config).unwrap();
    let command = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_openide-remote"))
            .arg("--state")
            .arg(&root)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(command(&["check"]).status.success());
    let output_path = root.join("pairing.json");
    let provisioned = command(&[
        "device-add",
        "iPhone smoke",
        "--output",
        output_path.to_str().unwrap(),
    ]);
    assert!(
        provisioned.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&provisioned.stderr)
    );
    let pairing: Value = storage::read_json(&output_path).unwrap();
    let token = pairing["token"].as_str().unwrap();
    assert!(!String::from_utf8_lossy(&provisioned.stdout).contains(token));
    assert!(DeviceStore::new(&root).authenticate(token));
    assert!(command(&["device-list"]).status.success());
    assert!(
        command(&["device-revoke", pairing["device_id"].as_str().unwrap()])
            .status
            .success()
    );
    assert!(!DeviceStore::new(&root).authenticate(token));
}

#[tokio::test]
async fn steering_retry_after_completion_acknowledges_the_original_input() {
    let f = Fixture::new(true).await;
    let session = f.session().await;
    f.request(
        "POST",
        &format!("/v1/sessions/{session}/turns"),
        Some(json!({"request_id":"steered-turn","prompt":"Propose a write","mode":"build"})),
    )
    .await;
    let view = f.wait(&session, "waiting_permission").await;
    let steer_url = format!("/v1/sessions/{session}/steer");
    let payload = json!({"request_id":"steer-one","turn_id":"steered-turn","prompt":"Report the final result briefly."});
    assert_eq!(
        f.request("POST", &steer_url, Some(payload.clone())).await.0,
        StatusCode::OK
    );
    let request_id = view["pending_permissions"][0]["request_id"]
        .as_str()
        .unwrap();
    f.request(
        "POST",
        &format!("/v1/sessions/{session}/permissions/{request_id}"),
        Some(json!({"decision":"deny"})),
    )
    .await;
    f.wait(&session, "completed").await;
    let (status, value) = f.request("POST", &steer_url, Some(payload)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["status"], "queued");
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    let (_, page) = f
        .request("GET", &format!("/v1/sessions/{session}/events"), None)
        .await;
    assert_eq!(
        page["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(
                |e| e["type"] == "user_message" && e["text"] == "Report the final result briefly."
            )
            .count(),
        1
    );
}
