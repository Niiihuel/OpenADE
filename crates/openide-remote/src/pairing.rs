//! Short-lived, one-use pairing invitations. Secrets are never part of a URL
//! query and completion tokens exist only in private retry state until ACK/TTL.
use crate::{auth::DeviceStore, storage};
use anyhow::{bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const INVITATION_TTL_SECONDS: u64 = 600;
const MAX_INVITATIONS: usize = 32;

#[derive(Debug)]
pub enum PairingError {
    InvalidRequest,
    Unauthorized,
    Expired,
    AlreadyUsed,
    Conflict,
    RateLimited,
}
impl fmt::Display for PairingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "invalid pairing request",
            Self::Unauthorized => "invalid pairing invitation",
            Self::Expired => "pairing invitation expired",
            Self::AlreadyUsed => "pairing invitation already used",
            Self::Conflict => "pairing retry does not match the original request",
            Self::RateLimited => "too many pairing attempts; retry after one minute",
        })
    }
}
impl std::error::Error for PairingError {}

/// What the QR code carries: where the PC answers, how to recognize it,
/// and a secret good once for ten minutes.
#[derive(Clone, Serialize, Deserialize)]
pub struct PairingInvitation {
    pub version: u32,
    /// `https://host:port` addresses, the likeliest first.
    pub routes: Vec<String>,
    /// SHA-256 of the PC's certificate, which the phone pins.
    pub fingerprint: String,
    pub server_id: String,
    pub pairing_id: String,
    pub secret: String,
    pub expires_at: u64,
    pub label: String,
}
impl PairingInvitation {
    pub fn deep_link(&self) -> Result<String> {
        Ok(format!(
            "openade-remote://pair#{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    /// The QR code's modules, row by row, `true` for dark.
    pub fn qr_modules(&self) -> Result<(usize, Vec<bool>)> {
        let code = qrcode::QrCode::new(self.deep_link()?.as_bytes())?;
        let modules = code.to_colors().into_iter().map(|color| color == qrcode::Color::Dark).collect();
        Ok((code.width(), modules))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub pairing_id: String,
    pub secret: String,
    pub request_id: String,
    pub device_name: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PairingResult {
    pub version: u32,
    pub device_id: String,
    pub device_name: String,
    pub token: String,
}
#[derive(Serialize, Deserialize)]
struct Completion {
    request_id: String,
    result: PairingResult,
}
#[derive(Serialize, Deserialize)]
struct Pending {
    pairing_id: String,
    secret_sha256: Vec<u8>,
    expires_at: u64,
    completion: Option<Completion>,
}

#[derive(Clone)]
pub struct PairingStore {
    root: PathBuf,
}
impl PairingStore {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
        }
    }
    fn directory(&self) -> PathBuf {
        self.root.join("pairing")
    }
    fn path(&self, id: &str) -> PathBuf {
        self.directory().join(format!("{id}.json"))
    }
    fn lock(&self) -> Result<fs::File> {
        storage::prepare_directory(&self.directory())?;
        storage::lock_file(&self.directory().join("pairing.lock"))
    }

    pub fn start(
        &self,
        routes: &[String],
        fingerprint: &str,
        server_id: &str,
        label: &str,
    ) -> Result<PairingInvitation> {
        if routes.is_empty() || routes.len() > 8 {
            return Err(PairingError::InvalidRequest.into());
        }
        let routes = routes.iter().map(|route| validate_base_url(route)).collect::<Result<Vec<_>>>()?;
        validate_name(label)?;
        let _lock = self.lock()?;
        self.purge_locked(now())?;
        let count = fs::read_dir(self.directory())?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
            .count();
        if count >= MAX_INVITATIONS {
            bail!("too many active pairing invitations");
        }
        let invitation = PairingInvitation {
            version: 2,
            routes,
            fingerprint: fingerprint.to_owned(),
            server_id: server_id.to_owned(),
            pairing_id: Uuid::new_v4().to_string(),
            secret: random_secret(),
            expires_at: now() + INVITATION_TTL_SECONDS,
            label: label.trim().to_owned(),
        };
        storage::write_json(
            &self.path(&invitation.pairing_id),
            &Pending {
                pairing_id: invitation.pairing_id.clone(),
                secret_sha256: Sha256::digest(invitation.secret.as_bytes()).to_vec(),
                expires_at: invitation.expires_at,
                completion: None,
            },
        )?;
        Ok(invitation)
    }

    pub fn complete(&self, request: PairRequest) -> Result<PairingResult> {
        admit_attempt()?;
        validate_uuid(&request.pairing_id)?;
        validate_uuid(&request.request_id)?;
        validate_name(&request.device_name)?;
        if request.secret.len() != 64 || !request.secret.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(PairingError::Unauthorized.into());
        }
        let _lock = self.lock()?;
        let path = self.path(&request.pairing_id);
        if !path.exists() {
            return Err(PairingError::Unauthorized.into());
        }
        let mut pending: Pending = storage::read_json(&path)?;
        let digest = Sha256::digest(request.secret.as_bytes());
        if pending.secret_sha256.len() != 32
            || !bool::from(pending.secret_sha256.as_slice().ct_eq(digest.as_slice()))
        {
            return Err(PairingError::Unauthorized.into());
        }
        if pending.expires_at <= now() {
            fs::remove_file(path)?;
            return Err(PairingError::Expired.into());
        }
        if let Some(completion) = &pending.completion {
            if completion.request_id != request.request_id {
                return Err(PairingError::AlreadyUsed.into());
            }
            if completion.result.device_name != request.device_name.trim() {
                return Err(PairingError::Conflict.into());
            }
        } else {
            // Write the exact response before device insertion. A crash/retry
            // resumes issuance of the same token rather than creating another.
            pending.completion = Some(Completion {
                request_id: request.request_id,
                result: PairingResult {
                    version: 2,
                    device_id: Uuid::new_v4().to_string(),
                    device_name: request.device_name.trim().to_owned(),
                    token: random_secret(),
                },
            });
            storage::write_json(&path, &pending)?;
        }
        let result = pending
            .completion
            .as_ref()
            .expect("completion prepared")
            .result
            .clone();
        DeviceStore::new(&self.root).issue_prepared(
            &result.device_name,
            &result.device_id,
            &result.token,
        )?;
        Ok(result)
    }

    pub fn ack(&self, pairing_id: &str, authenticated_device_id: &str) -> Result<()> {
        validate_uuid(pairing_id)?;
        validate_uuid(authenticated_device_id)?;
        let _lock = self.lock()?;
        let path = self.path(pairing_id);
        if !path.exists() {
            return Ok(());
        }
        let pending: Pending = storage::read_json(&path)?;
        if pending
            .completion
            .as_ref()
            .is_none_or(|c| c.result.device_id != authenticated_device_id)
        {
            return Err(PairingError::Unauthorized.into());
        }
        fs::remove_file(path)?;
        Ok(())
    }

    /// Local CLI rollback only. HTTP callers cannot cancel arbitrary invitations.
    pub fn cancel(&self, pairing_id: &str) -> Result<()> {
        validate_uuid(pairing_id)?;
        let _lock = self.lock()?;
        let path = self.path(pairing_id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn purge_expired(&self) -> Result<()> {
        let _lock = self.lock()?;
        self.purge_locked(now())
    }
    fn purge_locked(&self, timestamp: u64) -> Result<()> {
        for entry in fs::read_dir(self.directory())? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let pending: Pending = storage::read_json(&path)?;
                if pending.expires_at <= timestamp {
                    fs::remove_file(path)?;
                }
            }
        }
        Ok(())
    }
    pub fn has_active_invitation(&self) -> Result<bool> {
        let _lock = self.lock()?;
        self.purge_locked(now())?;
        for entry in fs::read_dir(self.directory())? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let pending: Pending = storage::read_json(&path)?;
                if pending.completion.is_none() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn validate_uuid(value: &str) -> Result<()> {
    if value.len() != 36 || Uuid::parse_str(value).is_err() {
        return Err(PairingError::InvalidRequest.into());
    }
    Ok(())
}
fn validate_name(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(PairingError::InvalidRequest.into());
    }
    Ok(())
}
fn validate_base_url(value: &str) -> Result<String> {
    let url = url::Url::parse(value).map_err(|_| PairingError::InvalidRequest)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(PairingError::InvalidRequest.into());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn admit_attempt() -> Result<()> {
    static BUDGET: OnceLock<Mutex<(Instant, u32)>> = OnceLock::new();
    let mut budget = BUDGET
        .get_or_init(|| Mutex::new((Instant::now(), 0)))
        .lock()
        .map_err(|_| PairingError::RateLimited)?;
    if budget.0.elapsed() >= Duration::from_secs(60) {
        *budget = (Instant::now(), 0);
    }
    if budget.1 >= 60 {
        return Err(PairingError::RateLimited.into());
    }
    budget.1 += 1;
    Ok(())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    const FINGERPRINT: &str = "ab";
    fn request(invitation: &PairingInvitation, request_id: &str) -> PairRequest {
        PairRequest {
            pairing_id: invitation.pairing_id.clone(),
            secret: invitation.secret.clone(),
            request_id: request_id.to_owned(),
            device_name: "My iPhone".into(),
        }
    }
    #[test]
    fn one_time_retry_ack_and_revocation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PairingStore::new(tmp.path());
        let invite = store.start(&["https://host.example".into()], FINGERPRINT, "server", "My PC").unwrap();
        let id = Uuid::new_v4().to_string();
        let first = store.complete(request(&invite, &id)).unwrap();
        let retry = store.complete(request(&invite, &id)).unwrap();
        assert_eq!(first.token, retry.token);
        assert_eq!(DeviceStore::new(tmp.path()).list().unwrap().len(), 1);
        assert!(store
            .complete(request(&invite, &Uuid::new_v4().to_string()))
            .is_err());
        assert!(store
            .ack(&invite.pairing_id, &Uuid::new_v4().to_string())
            .is_err());
        DeviceStore::new(tmp.path())
            .revoke(&first.device_id)
            .unwrap();
        assert!(store.complete(request(&invite, &id)).is_err());
        store.ack(&invite.pairing_id, &first.device_id).unwrap();
        assert!(!store.path(&invite.pairing_id).exists());
        assert!(store.complete(request(&invite, &id)).is_err());
    }
    #[test]
    fn secret_expiry_and_origin_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PairingStore::new(tmp.path());
        for url in [
            "http://host.example",
            "https://u:p@host.example",
            "https://host.example?token=x",
            "https://host.example/path",
        ] {
            assert!(store.start(&[url.into()], FINGERPRINT, "server", "PC").is_err());
        }
        assert!(store.start(&[], FINGERPRINT, "server", "PC").is_err());
        let invite = store
            .start(&["https://192.168.1.4:7787/".into(), "https://[2800::1]:7787".into()], FINGERPRINT, "server", "PC")
            .unwrap();
        assert_eq!(invite.routes, ["https://192.168.1.4:7787", "https://[2800::1]:7787"]);
        let mut req = request(&invite, &Uuid::new_v4().to_string());
        req.secret = "0".repeat(64);
        assert!(store.complete(req).is_err());
        let mut pending: Pending = storage::read_json(&store.path(&invite.pairing_id)).unwrap();
        pending.expires_at = 0;
        storage::write_json(&store.path(&invite.pairing_id), &pending).unwrap();
        store.purge_expired().unwrap();
        assert!(!store.path(&invite.pairing_id).exists());
        assert!(invite
            .deep_link()
            .unwrap()
            .starts_with("openade-remote://pair#"));
        assert!(!invite.deep_link().unwrap().contains('?'));
        let (width, modules) = invite.qr_modules().unwrap();
        assert_eq!(modules.len(), width * width);
    }

    #[test]
    fn retry_recovers_completion_persisted_before_device_insertion() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PairingStore::new(tmp.path());
        let invite = store.start(&["https://host.example".into()], FINGERPRINT, "server", "PC").unwrap();
        let request_id = Uuid::new_v4().to_string();
        let prepared = PairingResult {
            version: 2,
            device_id: Uuid::new_v4().to_string(),
            device_name: "My iPhone".into(),
            token: random_secret(),
        };
        let mut pending: Pending = storage::read_json(&store.path(&invite.pairing_id)).unwrap();
        pending.completion = Some(Completion {
            request_id: request_id.clone(),
            result: prepared.clone(),
        });
        storage::write_json(&store.path(&invite.pairing_id), &pending).unwrap();
        assert!(DeviceStore::new(tmp.path()).list().unwrap().is_empty());
        let recovered = store.complete(request(&invite, &request_id)).unwrap();
        assert_eq!(recovered.token, prepared.token);
        assert_eq!(recovered.device_id, prepared.device_id);
        assert!(DeviceStore::new(tmp.path()).authenticate(&recovered.token));
        let mut changed = request(&invite, &request_id);
        changed.device_name = "Another phone".into();
        assert!(store.complete(changed).is_err());
        assert_eq!(DeviceStore::new(tmp.path()).list().unwrap().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.path(&invite.pairing_id))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        store.ack(&invite.pairing_id, &recovered.device_id).unwrap();
        store.ack(&invite.pairing_id, &recovered.device_id).unwrap();
        assert!(!store.path(&invite.pairing_id).exists());
    }
}
