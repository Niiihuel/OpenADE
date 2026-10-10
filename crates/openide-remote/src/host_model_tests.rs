use super::*;
use crate::{
    api,
    auth::DeviceStore,
    config::{build_backend, ProviderConfig},
    models::{ModelCatalog, ModelChoice},
};
use axum::{body::Body, extract::State, http::Request, routing::post, Json, Router};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn request(
    app: &Router,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn model_and_effort_admission_is_exact_durable_and_reconciles_opaque_history() {
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let provider = Router::new().route("/v1/responses",post(|State(captured):State<Arc<Mutex<Vec<Value>>>>,Json(body):Json<Value>| async move {
        let number = { let mut calls=captured.lock().unwrap(); calls.push(body); calls.len() };
        let response=json!({"id":format!("response-{number}"),"status":"completed","output":[
            {"type":"reasoning","id":format!("reason-{number}"),"encrypted_content":format!("PRIVATE_OPAQUE_{number}"),"summary":[]},
            {"type":"message","id":format!("message-{number}"),"role":"assistant","status":"completed","content":[{"type":"output_text","text":"Portable response","annotations":[]}]}
        ],"usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}});
        ([("content-type","text/event-stream")],format!("data: {}\n\n",json!({"type":"response.completed","response":response})))
    })).with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, provider).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let config = Config {
        version: 1,
        bind: "127.0.0.1:0".parse().unwrap(),
        provider: ProviderConfig {
            id: "fixture".into(),
            protocol: "responses".into(),
            endpoint,
            model: "current".into(),
            api_key_env: None,
            desktop_data_root: None,
            tool_calls: true,
            image_input: false,
        },
        projects: vec![Project {
            id: "project".into(),
            name: "Fixture".into(),
            path: workspace,
        }],
        allow_write: true,
        allow_commands: false,
        command_isolation: openide_harness::CommandIsolation::HostUnrestricted,
    };
    let host = Host::open(
        &temp.path().join("state"),
        config.clone(),
        build_backend(&config.provider).unwrap(),
    )
    .unwrap();
    *host.catalog_override.lock().unwrap() = Some(ModelCatalog {
        default_model_id: "current".into(),
        selection_enabled: true,
        models: ["current", "alternate"]
            .into_iter()
            .map(|id| ModelChoice {
                id: id.into(),
                name: id.into(),
                provider: "fixture".into(),
                reasoning_efforts: vec!["low".into(), "high".into()],
                supports_images: false,
            })
            .collect(),
    });
    let (_, token) = DeviceStore::new(&host.root).issue("Test").unwrap();
    let app = api::router(host.clone());
    assert_eq!(
        request(&app, None, "GET", "/v1/models", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, catalog) = request(&app, Some(&token), "GET", "/v1/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["selection_enabled"], true);
    let (status, session) = request(
        &app,
        Some(&token),
        "POST",
        "/v1/sessions",
        json!({"project_id":"project","request_id":"create"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = session["id"].as_str().unwrap();
    let path = format!("/v1/sessions/{session}/turns");
    let first = json!({"request_id":"first","prompt":"First","mode":"plan","model_id":"current","reasoning_effort":"low"});
    let turns = [
        first.clone(),
        json!({"request_id":"second","prompt":"Second","mode":"plan","model_id":"alternate","reasoning_effort":"high"}),
        json!({"request_id":"third","prompt":"Third","mode":"plan","model_id":"alternate","reasoning_effort":"high"}),
        json!({"request_id":"fourth","prompt":"Fourth","mode":"plan"}),
    ];
    for turn in &turns {
        let (status, value) = request(&app, Some(&token), "POST", &path, turn.clone()).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (_, state) = request(
                    &app,
                    Some(&token),
                    "GET",
                    &format!("/v1/sessions/{session}"),
                    Value::Null,
                )
                .await;
                assert_ne!(state["status"], "failed", "{state}");
                if state["status"] == "completed" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    {
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0]["model"], "current");
        assert_eq!(requests[0]["reasoning"]["effort"], "low");
        assert_eq!(requests[1]["model"], "alternate");
        assert_eq!(requests[1]["reasoning"]["effort"], "high");
        assert!(!requests[1]["input"].to_string().contains("PRIVATE_OPAQUE"));
        assert!(requests[1]["input"]
            .to_string()
            .contains("Portable response"));
        assert!(requests[2]["input"]
            .to_string()
            .contains("PRIVATE_OPAQUE_2"));
        assert!(!requests[3]["input"].to_string().contains("PRIVATE_OPAQUE"));
        assert!(requests[3].get("reasoning").is_none());
    }
    // Losing capability metadata after an accepted turn never causes a second run.
    *host.catalog_override.lock().unwrap() = None;
    let (status, ack) = request(&app, Some(&token), "POST", &path, first.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ack["status"], "completed");
    let mut changed = first;
    changed["reasoning_effort"] = json!("high");
    let (status, error) = request(&app, Some(&token), "POST", &path, changed).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "idempotency_conflict");
    for rejected in [
        json!({"request_id":"bad-model","prompt":"No","mode":"build","model_id":"unlisted"}),
        json!({"request_id":"bad-effort","prompt":"No","mode":"build","reasoning_effort":"high"}),
        json!({"request_id":"bypass","prompt":"No","mode":"build","permission_mode":"full_access"}),
    ] {
        let expected = if rejected.get("permission_mode").is_some() {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::BAD_REQUEST
        };
        assert_eq!(
            request(&app, Some(&token), "POST", &path, rejected).await.0,
            expected
        );
    }
    assert_eq!(captured.lock().unwrap().len(), 4);
    host.shutdown();
    server.abort();
}
