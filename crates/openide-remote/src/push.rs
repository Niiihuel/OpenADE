//! Direct APNs delivery. Provider keys and device tokens stay in private host
//! state; notification bodies never contain prompts, output or project names.
use crate::{auth::DeviceStore, storage};
use anyhow::{anyhow, bail, Context, Result};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const APNS_TOPIC: &str = "dev.openade.remote";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApnsEnvironment {
    Production,
    Sandbox,
}
impl ApnsEnvironment {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Production => "https://api.push.apple.com",
            Self::Sandbox => "https://api.sandbox.push.apple.com",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushRegistrationRequest {
    pub apns_token: String,
    pub environment: ApnsEnvironment,
    pub bundle_id: String,
}
#[derive(Serialize)]
pub struct RegistrationStatus {
    pub registered: bool,
    pub device_id: String,
    pub environment: Option<ApnsEnvironment>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Registration {
    device_id: String,
    apns_token: String,
    environment: ApnsEnvironment,
}

#[derive(Clone)]
pub struct PushRegistry {
    root: PathBuf,
}
impl PushRegistry {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
        }
    }
    fn directory(&self) -> PathBuf {
        self.root.join("push")
    }
    fn path(&self) -> PathBuf {
        self.directory().join("devices.json")
    }
    fn lock(&self) -> Result<std::fs::File> {
        storage::prepare_directory(&self.directory())?;
        storage::lock_file(&self.directory().join("devices.lock"))
    }
    fn read(&self) -> Result<Vec<Registration>> {
        if !self.path().exists() {
            return Ok(Vec::new());
        }
        storage::read_json(&self.path())
    }
    pub fn register(
        &self,
        authenticated_device_id: &str,
        request: PushRegistrationRequest,
    ) -> Result<RegistrationStatus> {
        storage::validate_id(authenticated_device_id)?;
        if request.bundle_id != APNS_TOPIC
            || request.apns_token.len() < 32
            || request.apns_token.len() > 512
            || !request.apns_token.len().is_multiple_of(2)
            || !request.apns_token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("invalid APNs registration");
        }
        let _lock = self.lock()?;
        let mut registrations = self.read()?;
        registrations.retain(|r| r.device_id != authenticated_device_id);
        if registrations.len() >= 128 {
            bail!("push device limit reached");
        }
        registrations.push(Registration {
            device_id: authenticated_device_id.to_owned(),
            apns_token: request.apns_token.to_ascii_lowercase(),
            environment: request.environment,
        });
        storage::write_json(&self.path(), &registrations)?;
        Ok(RegistrationStatus {
            registered: true,
            device_id: authenticated_device_id.to_owned(),
            environment: Some(request.environment),
        })
    }
    pub fn remove(&self, authenticated_device_id: &str) -> Result<RegistrationStatus> {
        storage::validate_id(authenticated_device_id)?;
        let _lock = self.lock()?;
        let mut registrations = self.read()?;
        registrations.retain(|r| r.device_id != authenticated_device_id);
        storage::write_json(&self.path(), &registrations)?;
        Ok(RegistrationStatus {
            registered: false,
            device_id: authenticated_device_id.to_owned(),
            environment: None,
        })
    }
    fn remove_if_unchanged(&self, stale: &Registration) -> Result<()> {
        let _lock = self.lock()?;
        let mut registrations = self.read()?;
        registrations.retain(|r| {
            r.device_id != stale.device_id
                || r.apns_token != stale.apns_token
                || r.environment != stale.environment
        });
        storage::write_json(&self.path(), &registrations)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PushConfig {
    pub enabled: bool,
    pub team_id: String,
    pub key_id: String,
    pub topic: String,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PushEventKind {
    ApprovalRequired,
    TurnCompleted,
}
#[derive(Clone)]
pub struct PushEvent {
    pub kind: PushEventKind,
    pub session_id: String,
}
impl PushEvent {
    fn payload(&self, device_id: &str) -> serde_json::Value {
        let body = match self.kind {
            PushEventKind::ApprovalRequired => "Tu tarea requiere permiso.",
            PushEventKind::TurnCompleted => "Tu tarea terminó.",
        };
        json!({"aps":{"alert":{"title":"openADE Remote","body":body},"sound":"default"},
            "openade":{"version":1,"event":self.kind,"session_id":self.session_id,"device_id":device_id}})
    }
}
#[derive(Default, Debug, Serialize)]
pub struct DeliveryReport {
    pub delivered: usize,
    pub failed: usize,
    pub removed_invalid: usize,
}
#[derive(Serialize)]
struct Claims {
    iss: String,
    iat: u64,
}

pub struct PushService {
    root: PathBuf,
    registry: PushRegistry,
    config: PushConfig,
    signing_key: EncodingKey,
    jwt: Mutex<Option<(Instant, String)>>,
    client: reqwest::Client,
    #[cfg(test)]
    endpoint_override: Option<String>,
}
impl PushService {
    /// Absence/disabled config means push is unavailable, never a fake success.
    /// Malformed credentials are returned as an error to the trusted host.
    pub fn open(root: &Path) -> Result<Option<Self>> {
        let directory = root.join("push");
        let path = directory.join("config.json");
        if !path.exists() {
            return Ok(None);
        }
        let config: PushConfig = storage::read_json(&path)?;
        if !config.enabled {
            return Ok(None);
        }
        for id in [&config.key_id, &config.team_id] {
            if id.len() != 10
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            {
                bail!("invalid APNs credential identifier");
            }
        }
        if config.topic != APNS_TOPIC {
            bail!("APNs topic must be the native app bundle identifier");
        }
        let key = storage::read_private(&directory.join("apns.p8"), 16 * 1024)?;
        let signing_key =
            EncodingKey::from_ec_pem(&key).map_err(|_| anyhow!("invalid APNs signing key"))?;
        let client = reqwest::Client::builder()
            .https_only(true)
            .http2_prior_knowledge()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("cannot create APNs HTTP/2 client")?;
        Ok(Some(Self {
            root: root.to_owned(),
            registry: PushRegistry::new(root),
            config,
            signing_key,
            jwt: Mutex::new(None),
            client,
            #[cfg(test)]
            endpoint_override: None,
        }))
    }
    fn token(&self) -> Result<String> {
        let mut cache = self
            .jwt
            .lock()
            .map_err(|_| anyhow!("APNs token cache unavailable"))?;
        if let Some((issued, token)) = &*cache {
            if issued.elapsed() < Duration::from_secs(50 * 60) {
                return Ok(token.clone());
            }
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.config.key_id.clone());
        let token = encode(
            &header,
            &Claims {
                iss: self.config.team_id.clone(),
                iat: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            },
            &self.signing_key,
        )
        .map_err(|_| anyhow!("cannot sign APNs provider token"))?;
        *cache = Some((Instant::now(), token.clone()));
        Ok(token)
    }

    /// Intended to run outside the turn's critical path. Authentication is
    /// rechecked per delivery so revoked device credentials receive no push.
    pub async fn send_event(&self, event: PushEvent) -> Result<DeliveryReport> {
        storage::validate_id(&event.session_id)?;
        let registrations = self.registry.read()?;
        let mut report = DeliveryReport::default();
        for registration in registrations {
            let active = DeviceStore::new(&self.root)
                .list()?
                .iter()
                .any(|d| d.id == registration.device_id && !d.revoked);
            if !active {
                self.registry.remove_if_unchanged(&registration)?;
                continue;
            }
            let payload = event.payload(&registration.device_id);
            let jwt = self.token()?;
            let mut delivered = false;
            for attempt in 0..3 {
                let response = self
                    .client
                    .post(format!(
                        "{}/3/device/{}",
                        self.endpoint(registration.environment),
                        registration.apns_token
                    ))
                    .bearer_auth(&jwt)
                    .header("apns-topic", APNS_TOPIC)
                    .header("apns-push-type", "alert")
                    .header("apns-priority", "10")
                    .header("apns-expiration", "0")
                    .json(&payload)
                    .send()
                    .await;
                // Never display reqwest errors: they can contain the device token URL.
                let Ok(response) = response else {
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))).await;
                        continue;
                    }
                    break;
                };
                let status = response.status();
                if status.is_success() {
                    report.delivered += 1;
                    delivered = true;
                    break;
                }
                if status.as_u16() == 410 {
                    self.registry.remove_if_unchanged(&registration)?;
                    report.removed_invalid += 1;
                    break;
                }
                if status.as_u16() == 400 {
                    let reason = response.json::<serde_json::Value>().await.ok();
                    if reason
                        .as_ref()
                        .and_then(|v| v.get("reason"))
                        .and_then(|v| v.as_str())
                        == Some("BadDeviceToken")
                    {
                        self.registry.remove_if_unchanged(&registration)?;
                        report.removed_invalid += 1;
                    }
                    break;
                }
                if (status.as_u16() == 429 || status.is_server_error()) && attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))).await;
                    continue;
                }
                break;
            }
            if !delivered {
                report.failed += 1;
            }
        }
        Ok(report)
    }

    fn endpoint(&self, environment: ApnsEnvironment) -> &str {
        #[cfg(test)]
        if let Some(endpoint) = &self.endpoint_override {
            return endpoint;
        }
        environment.endpoint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registration(token: &str) -> PushRegistrationRequest {
        PushRegistrationRequest {
            apns_token: token.into(),
            environment: ApnsEnvironment::Production,
            bundle_id: APNS_TOPIC.into(),
        }
    }
    #[test]
    fn registration_replaces_only_authenticated_device_and_stale_response_cannot_remove_new_token()
    {
        let tmp = tempfile::tempdir().unwrap();
        let registry = PushRegistry::new(tmp.path());
        registry
            .register("phone-a", registration(&"a".repeat(64)))
            .unwrap();
        registry
            .register("phone-b", registration(&"b".repeat(64)))
            .unwrap();
        let stale = registry.read().unwrap().remove(0);
        registry
            .register("phone-a", registration(&"c".repeat(64)))
            .unwrap();
        registry.remove_if_unchanged(&stale).unwrap();
        assert_eq!(registry.read().unwrap().len(), 2);
        registry.remove("phone-a").unwrap();
        let remaining = registry.read().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].device_id, "phone-b");
        let mut invalid = registration(&"a".repeat(64));
        invalid.bundle_id = "other.app".into();
        assert!(registry.register("phone-a", invalid).is_err());
        assert!(registry
            .register("phone-a", registration("invalid-token"))
            .is_err());
    }
    #[test]
    fn absent_configuration_is_unavailable_and_payload_is_generic() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(PushService::open(tmp.path()).unwrap().is_none());
        let payload = PushEvent {
            kind: PushEventKind::ApprovalRequired,
            session_id: "session-1".into(),
        }
        .payload("device-1");
        assert_eq!(
            payload["aps"]["alert"]["body"],
            "Tu tarea requiere permiso."
        );
        assert_eq!(payload["openade"]["session_id"], "session-1");
        assert_eq!(payload.as_object().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn http2_delivery_headers_payload_invalid_token_cleanup_and_revocation() {
        use axum::{
            body::to_bytes,
            extract::{Request, State},
            http::{StatusCode, Version},
            routing::post,
            Router,
        };
        use std::sync::Arc;
        type Captured = Arc<Mutex<Vec<(Version, axum::http::HeaderMap, serde_json::Value)>>>;
        async fn receive(State(captured): State<Captured>, request: Request) -> StatusCode {
            let status = if request.uri().path().ends_with(&"b".repeat(64)) {
                StatusCode::GONE
            } else {
                StatusCode::OK
            };
            let version = request.version();
            let headers = request.headers().clone();
            let body = to_bytes(request.into_body(), 4096).await.unwrap();
            captured.lock().unwrap().push((
                version,
                headers,
                serde_json::from_slice(&body).unwrap(),
            ));
            status
        }
        let captured: Captured = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/3/device/{token}", post(receive))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let tmp = tempfile::tempdir().unwrap();
        let registry = PushRegistry::new(tmp.path());
        let devices = DeviceStore::new(tmp.path());
        let (first, _) = devices.issue("phone-a").unwrap();
        let (second, _) = devices.issue("phone-b").unwrap();
        registry
            .register(&first.id, registration(&"a".repeat(64)))
            .unwrap();
        registry
            .register(&second.id, registration(&"b".repeat(64)))
            .unwrap();
        let service = PushService {
            root: tmp.path().to_owned(),
            registry: registry.clone(),
            config: PushConfig {
                enabled: true,
                team_id: "TESTTEAM01".into(),
                key_id: "TESTKEY001".into(),
                topic: APNS_TOPIC.into(),
            },
            signing_key: EncodingKey::from_secret(b"unused-cache-fixture"),
            jwt: Mutex::new(Some((Instant::now(), "provider-fixture".into()))),
            client: reqwest::Client::builder()
                .no_proxy()
                .http2_prior_knowledge()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            endpoint_override: Some(format!("http://{address}")),
        };
        let event = PushEvent {
            kind: PushEventKind::ApprovalRequired,
            session_id: "session-safe-id".into(),
        };
        let report = service.send_event(event.clone()).await.unwrap();
        assert_eq!(
            (report.delivered, report.failed, report.removed_invalid),
            (1, 1, 1)
        );
        assert_eq!(registry.read().unwrap().len(), 1);
        {
            let requests = captured.lock().unwrap();
            assert_eq!(requests.len(), 2);
            for (version, headers, body) in requests.iter() {
                assert_eq!(*version, Version::HTTP_2);
                assert_eq!(headers["authorization"], "Bearer provider-fixture");
                assert_eq!(headers["apns-topic"], APNS_TOPIC);
                assert_eq!(headers["apns-push-type"], "alert");
                assert_eq!(body["aps"]["alert"]["body"], "Tu tarea requiere permiso.");
                assert!(
                    body["openade"]["device_id"] == first.id
                        || body["openade"]["device_id"] == second.id
                );
            }
        }
        devices.revoke(&first.id).unwrap();
        let report = service.send_event(event).await.unwrap();
        assert_eq!(report.delivered, 0);
        assert_eq!(captured.lock().unwrap().len(), 2);
        assert!(registry.read().unwrap().is_empty());
        server.abort();
    }
}
