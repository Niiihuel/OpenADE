use super::*;
use serde_json::json;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

fn config(root: &Path) -> ProviderConfig {
    ProviderConfig {
        id: "codex".into(),
        protocol: "responses".into(),
        endpoint: "https://api.openai.com/v1".into(),
        model: "current".into(),
        api_key_env: None,
        desktop_data_root: Some(root.to_owned()),
        tool_calls: true,
        image_input: true,
    }
}

fn document() -> Vec<u8> {
    serde_json::to_vec(&json!({"private_account_field":"MUST_NOT_PERSIST", "models":[
        {"slug":"current","display_name":"Current","visibility":"list",
         "supported_reasoning_levels":[{"effort":"low","description":"not projected"},{"effort":"high"}],
         "input_modalities":["text","image"],"context_window":100000},
        {"slug":"alternate","display_name":"Alternate","visibility":"list",
         "supported_reasoning_levels":[{"effort":"medium"}],"input_modalities":["text"]},
        {"slug":"hidden","display_name":"Hidden","visibility":"hide",
         "supported_reasoning_levels":[{"effort":"max"}]},
        {"slug":"unspecified","display_name":"No visibility"}
    ]})).unwrap()
}

fn server(status: &str, headers: &str, body: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/models", listener.local_addr().unwrap());
    let response = format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n");
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            if bytes.ends_with(b"\r\n\r\n") {
                break;
            }
            assert!(bytes.len() < 16384);
        }
        let request = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /v1/models http/1.1\r\n"));
        assert!(request.contains("authorization: bearer fake-account-token\r\n"));
        assert!(request.contains("accept: application/json\r\n"));
        stream.write_all(response.as_bytes()).unwrap();
        // The client may close immediately on an oversized Content-Length.
        let _ = stream.write_all(&body);
    });
    (url, worker)
}

#[test]
fn official_get_projection_matches_maintained_desktop_codex_schema() {
    let definitions: Value = serde_json::from_str(include_str!(
        "../../../apps/agents-window/src/provider-definitions.json"
    ))
    .unwrap();
    let adapter = definitions["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "codex")
        .unwrap();
    assert_eq!(
        format!(
            "{}/{}",
            adapter["endpoint"].as_str().unwrap(),
            adapter["model_list_path"].as_str().unwrap()
        ),
        ENDPOINT
    );
    for (key, value) in [
        ("entries_path", "models"),
        ("id_path", "slug"),
        ("name_path", "display_name"),
        ("visibility_path", "visibility"),
        ("visibility_value", "list"),
        ("effort_levels_path", "supported_reasoning_levels"),
        ("input_modalities_path", "input_modalities"),
        ("effort_format", "preset_array"),
    ] {
        assert_eq!(adapter["catalog"][key], value);
    }
    let body = document();
    let (url, worker) = server(
        "200 OK",
        &format!("Content-Length: {}\r\n", body.len()),
        body,
    );
    let bytes = fetch(&url, "fake-account-token").unwrap();
    worker.join().unwrap();
    let cache = parse_codex(&bytes, "fake-account-token", 100).unwrap();
    let projected = serde_json::to_vec(&cache).unwrap();
    assert!(!String::from_utf8_lossy(&projected).contains("MUST_NOT_PERSIST"));
    assert!(!String::from_utf8_lossy(&projected).contains("fake-account-token"));
    let temp = tempfile::tempdir().unwrap();
    let catalog = from_verified_cache(
        &config(temp.path()),
        &projected,
        &credential_fingerprint("fake-account-token"),
        101,
    )
    .unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["current", "alternate"]
    );
    assert_eq!(catalog.models[0].reasoning_efforts, ["low", "high"]);
    assert!(catalog.models[0].supports_images);
    assert!(!catalog.models[1].supports_images);
}

#[test]
fn refresh_reuses_cache_and_replaces_it_after_credential_rotation() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    let refresh = CatalogRefresh::default();
    let calls = AtomicUsize::new(0);
    let load = || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(document())
    };
    refresh.refresh_with(&config, temp.path(), || Ok("first".into()), |_| load());
    assert!(
        ModelCatalog::load_for_key(&config, temp.path(), "first", now_ms())
            .unwrap()
            .selection_enabled
    );
    refresh.refresh_with(&config, temp.path(), || Ok("first".into()), |_| load());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(ModelCatalog::load_for_key(&config, temp.path(), "rotated", now_ms()).is_none());
    refresh.refresh_with(&config, temp.path(), || Ok("rotated".into()), |_| load());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(ModelCatalog::load_for_key(&config, temp.path(), "first", now_ms()).is_none());
    assert!(ModelCatalog::load_for_key(&config, temp.path(), "rotated", now_ms()).is_some());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(temp.path().join(CACHE_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(!temp.path().join("provider-catalog.json").exists());
}

#[test]
fn failures_preserve_old_snapshot_without_reusing_another_credentials_choices() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    let refresh = CatalogRefresh::default();
    refresh.refresh_with(
        &config,
        temp.path(),
        || Ok("first".into()),
        |_| Ok(document()),
    );
    let old = std::fs::read(temp.path().join(CACHE_FILE)).unwrap();
    refresh.refresh_with(
        &config,
        temp.path(),
        || Ok("other-account".into()),
        |_| Err(()),
    );
    assert_eq!(std::fs::read(temp.path().join(CACHE_FILE)).unwrap(), old);
    assert!(ModelCatalog::load_for_key(&config, temp.path(), "other-account", now_ms()).is_none());
    refresh.refresh_with(
        &config,
        temp.path(),
        || panic!("failure cooldown must avoid OAuth"),
        |_| panic!("failure cooldown must avoid requests"),
    );
    let mut wrong = config.clone();
    wrong.endpoint = "https://untrusted.invalid/v1".into();
    CatalogRefresh::default().refresh_with(
        &wrong,
        temp.path(),
        || panic!("unsupported route"),
        |_| panic!("unsupported route"),
    );
    let blocked = CatalogRefresh::default();
    let _inflight = blocked.failed_at.lock().unwrap();
    blocked.refresh_with(
        &config,
        temp.path(),
        || panic!("must not wait for inflight request"),
        |_| panic!("duplicate refresh"),
    );
}

#[test]
fn malformed_metadata_and_http_errors_do_not_authorize_choices() {
    let mut doc: Value = serde_json::from_slice(&document()).unwrap();
    doc["models"][0]["supported_reasoning_levels"] = json!([{"effort":"high"},{"effort":"high"}]);
    doc["models"][0]["input_modalities"] = json!(["image", 5]);
    let cache = parse_codex(&serde_json::to_vec(&doc).unwrap(), "fake", 100).unwrap();
    assert!(cache.providers["codex"].models["current"]
        .provider_efforts
        .is_empty());
    assert!(cache.providers["codex"].models["current"]
        .declared_capabilities
        .is_empty());
    doc["models"][1]["slug"] = json!("current");
    assert!(parse_codex(&serde_json::to_vec(&doc).unwrap(), "fake", 100).is_err());
    assert!(parse_codex(
        br#"{"data":[{"id":"unverified-openai-api-key-route"}]}"#,
        "fake",
        100
    )
    .is_err());
    for (status, headers) in [
        (
            "302 Found",
            "Location: http://127.0.0.1:1/never\r\n".to_owned(),
        ),
        ("401 Unauthorized", "".to_owned()),
        (
            "200 OK",
            format!("Content-Length: {}\r\n", MAX_RESPONSE_BYTES + 1),
        ),
    ] {
        let (url, worker) = server(status, &headers, Vec::new());
        assert!(fetch(&url, "fake-account-token").is_err());
        worker.join().unwrap();
    }
}

#[test]
fn account_binding_allows_token_rotation_but_rejects_an_account_switch() {
    let temp = tempfile::tempdir().unwrap();
    let config = config(temp.path());
    let mut record = openide_connections::Credential {
        version: 1,
        provider_id: "codex".into(),
        access_token: "first".into(),
        subject: Some("same-account".into()),
        client_id: Some("same-client".into()),
        oauth: true,
        ..Default::default()
    };
    openide_connections::save(temp.path(), &record).unwrap();
    let refresh = CatalogRefresh::new(&config);
    assert!(refresh.account_matches(&config));
    record.access_token = "rotated".into();
    openide_connections::save(temp.path(), &record).unwrap();
    assert!(refresh.account_matches(&config));
    record.subject = Some("different-account".into());
    openide_connections::save(temp.path(), &record).unwrap();
    assert!(!refresh.account_matches(&config));
    assert!(!refresh.load(&config, temp.path()).selection_enabled);
    refresh.refresh_with(
        &config,
        temp.path(),
        || panic!("changed account must not renew"),
        |_| panic!("changed account must not fetch"),
    );
    let restarted = CatalogRefresh::new(&config);
    assert!(restarted.account_matches(&config));
    record.disabled = true;
    openide_connections::save(temp.path(), &record).unwrap();
    assert!(!restarted.account_matches(&config));
}
