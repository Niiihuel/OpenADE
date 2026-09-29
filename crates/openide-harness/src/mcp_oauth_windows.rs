//! Persistent MCP OAuth tokens in the current Windows user's Credential Manager.
//!
//! The generic credential target contains only a hash of the security
//! principals. Token values are confined to the credential blob.

use crate::mcp_oauth::{grant_identity_digest, OAuthError, OAuthTokens, TokenKey, TokenStore};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::sync::atomic::{compiler_fence, Ordering};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NOT_FOUND, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_MAX_CREDENTIAL_BLOB_SIZE,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

const TARGET_PREFIX: &str = "OpenIDE:MCP:OAuth:v1:";
const RESUME_HMAC_KEY_TARGET: &str = "OpenIDE:MCP:ResumeHmac:v1";
const BLOB_VERSION: u8 = 1;
const CREDENTIAL_LOCK_TIMEOUT_MS: u32 = 10_000;

/// Persist tokens for the current Windows user on this machine. Other
/// processes running as that user can read the generic credential; this is
/// the security boundary provided by Windows Credential Manager.
#[derive(Default)]
pub struct WindowsCredentialTokenStore;

impl WindowsCredentialTokenStore {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTokens {
    version: u8,
    #[serde(default)]
    grant_revision: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
    expires_at_unix_seconds: Option<u64>,
    scopes: Vec<String>,
}

impl StoredTokens {
    fn new(tokens: OAuthTokens, grant_revision: Option<String>) -> Self {
        Self {
            version: BLOB_VERSION,
            grant_revision,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at_unix_seconds: tokens.expires_at_unix_seconds,
            scopes: tokens.scopes,
        }
    }
}

impl TryFrom<StoredTokens> for OAuthTokens {
    type Error = OAuthError;

    fn try_from(stored: StoredTokens) -> Result<Self, Self::Error> {
        if stored.version != BLOB_VERSION {
            return Err(OAuthError::Store);
        }
        Ok(Self {
            access_token: stored.access_token,
            refresh_token: stored.refresh_token,
            expires_at_unix_seconds: stored.expires_at_unix_seconds,
            scopes: stored.scopes,
        })
    }
}

struct SensitiveBytes(Vec<u8>);

impl Drop for SensitiveBytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // A regular fill can be optimized away before deallocation.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
fn serialize_tokens(tokens: OAuthTokens) -> Result<SensitiveBytes, OAuthError> {
    serialize_record(&StoredTokens::new(tokens, Some(new_grant_revision())))
}

fn serialize_record(record: &StoredTokens) -> Result<SensitiveBytes, OAuthError> {
    let bytes = SensitiveBytes(serde_json::to_vec(record).map_err(|_| OAuthError::Store)?);
    if bytes.0.is_empty() || bytes.0.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(OAuthError::Store);
    }
    Ok(bytes)
}

#[cfg(test)]
fn deserialize_tokens(bytes: &[u8]) -> Result<OAuthTokens, OAuthError> {
    deserialize_record(bytes)?.try_into()
}

fn deserialize_record(bytes: &[u8]) -> Result<StoredTokens, OAuthError> {
    if bytes.is_empty() || bytes.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(OAuthError::Store);
    }
    let stored: StoredTokens = serde_json::from_slice(bytes).map_err(|_| OAuthError::Store)?;
    if stored.version != BLOB_VERSION
        || stored.grant_revision.as_ref().is_some_and(|revision| {
            revision.len() != 64 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(OAuthError::Store);
    }
    Ok(stored)
}

fn new_grant_revision() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn target_name(key: &TokenKey) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"openide:mcp:oauth:target:v1\0");
    for part in [&key.resource, &key.issuer, &key.client_id] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let mut target = String::with_capacity(TARGET_PREFIX.len() + 64);
    target.push_str(TARGET_PREFIX);
    for byte in hasher.finalize() {
        write!(&mut target, "{byte:02x}").expect("writing hex into String cannot fail");
    }
    target
}

fn wide_target(key: &TokenKey) -> Vec<u16> {
    target_name(key).encode_utf16().chain(Some(0)).collect()
}

struct CredentialBuffer(*mut CREDENTIALW);

impl Drop for CredentialBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CredFree(self.0.cast()) };
        }
    }
}

struct CredentialLock(HANDLE);

impl Drop for CredentialLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

fn lock_credential(key: &TokenKey) -> Result<CredentialLock, OAuthError> {
    let target = target_name(key);
    lock_named(&format!(
        "Local\\OpenIDE-MCP-OAuth-{}",
        &target[TARGET_PREFIX.len()..]
    ))
}

fn lock_named(name: &str) -> Result<CredentialLock, OAuthError> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(OAuthError::Store);
    }
    let wait = unsafe { WaitForSingleObject(handle, CREDENTIAL_LOCK_TIMEOUT_MS) };
    if wait != WAIT_OBJECT_0 {
        if wait == WAIT_ABANDONED {
            unsafe { ReleaseMutex(handle) };
        }
        unsafe { CloseHandle(handle) };
        return Err(OAuthError::Store);
    }
    Ok(CredentialLock(handle))
}

fn read_resume_hmac_key() -> Option<[u8; 32]> {
    let target: Vec<u16> = RESUME_HMAC_KEY_TARGET
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut pointer = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) } == 0 {
        return None;
    }
    let buffer = CredentialBuffer(pointer);
    if pointer.is_null() {
        return None;
    }
    let credential = unsafe { &*buffer.0 };
    if credential.CredentialBlobSize != 32 || credential.CredentialBlob.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(credential.CredentialBlob, 32) };
    let mut key = [0u8; 32];
    key.copy_from_slice(bytes);
    wipe(bytes);
    Some(key)
}

fn write_resume_hmac_key(key: &mut [u8; 32]) -> bool {
    let mut target: Vec<u16> = RESUME_HMAC_KEY_TARGET
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: key.len() as u32,
        CredentialBlob: key.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    (unsafe { CredWriteW(&credential, 0) }) != 0
}

fn hmac_sha256(key: &[u8; 32], value: &[u8]) -> String {
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(b"openide-mcp-bearer-resume-v1\0");
    inner.update(value);
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner.finalize());
    let digest = format!("{:x}", outer.finalize());
    wipe(&mut inner_pad);
    wipe(&mut outer_pad);
    digest
}

fn bearer_resume_identity_sync(token: &str) -> Option<String> {
    let _lock = lock_named("Local\\OpenIDE-MCP-ResumeHmac-v1").ok()?;
    let mut key = if let Some(key) = read_resume_hmac_key() {
        key
    } else {
        let mut generated = [0u8; 32];
        generated[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        generated[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        if !write_resume_hmac_key(&mut generated) {
            wipe(&mut generated);
            return None;
        }
        generated
    };
    let digest = hmac_sha256(&key, token.as_bytes());
    wipe(&mut key);
    Some(digest)
}

/// Stable across process restarts for an unchanged bearer token. Only the
/// HMAC is journaled; its random key remains in Windows Credential Manager.
pub(crate) async fn bearer_resume_identity(token: String) -> Option<String> {
    tokio::task::spawn_blocking(move || bearer_resume_identity_sync(&token))
        .await
        .ok()
        .flatten()
}

fn load_record_sync(key: &TokenKey) -> Result<Option<StoredTokens>, OAuthError> {
    let target = wide_target(key);
    let mut pointer = std::ptr::null_mut();
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) };
    if ok == 0 {
        return if unsafe { GetLastError() } == ERROR_NOT_FOUND {
            Ok(None)
        } else {
            Err(OAuthError::Store)
        };
    }
    let buffer = CredentialBuffer(pointer);
    if pointer.is_null() {
        return Err(OAuthError::Store);
    }
    let credential = unsafe { &*buffer.0 };
    let len = credential.CredentialBlobSize as usize;
    if len == 0
        || len > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize
        || credential.CredentialBlob.is_null()
    {
        return Err(OAuthError::Store);
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(credential.CredentialBlob, len) };
    let result = deserialize_record(bytes);
    wipe(bytes);
    result.map(Some)
}

fn load_sync(key: &TokenKey) -> Result<Option<OAuthTokens>, OAuthError> {
    load_record_sync(key)?.map(TryInto::try_into).transpose()
}

fn write_record_sync(key: &TokenKey, record: &StoredTokens) -> Result<(), OAuthError> {
    let mut target = wide_target(key);
    let mut bytes = serialize_record(record)?;
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: bytes.0.len() as u32,
        CredentialBlob: bytes.0.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    if unsafe { CredWriteW(&credential, 0) } == 0 {
        return Err(OAuthError::Store);
    }
    Ok(())
}

fn save_sync(key: &TokenKey, tokens: OAuthTokens) -> Result<(), OAuthError> {
    let _lock = lock_credential(key)?;
    write_record_sync(key, &StoredTokens::new(tokens, Some(new_grant_revision())))
}

fn save_refreshed_sync(
    key: &TokenKey,
    previous: &OAuthTokens,
    tokens: OAuthTokens,
) -> Result<(), OAuthError> {
    let _lock = lock_credential(key)?;
    let current = load_record_sync(key)?.ok_or(OAuthError::AuthorizationRequired)?;
    let revision = current.grant_revision.clone();
    if &OAuthTokens::try_from(current)? != previous {
        return Err(OAuthError::AuthorizationRequired);
    }
    write_record_sync(key, &StoredTokens::new(tokens, revision))
}

fn resume_grant_identity_sync(key: &TokenKey) -> Result<Option<String>, OAuthError> {
    let Some(record) = load_record_sync(key)? else {
        return Ok(None);
    };
    Ok(record
        .grant_revision
        .as_deref()
        .map(|revision| grant_identity_digest(key, revision, &record.scopes)))
}

fn delete_sync(key: &TokenKey) -> Result<(), OAuthError> {
    let _lock = lock_credential(key)?;
    delete_record_sync(key)
}

fn delete_record_sync(key: &TokenKey) -> Result<(), OAuthError> {
    let target = wide_target(key);
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0
        && unsafe { GetLastError() } != ERROR_NOT_FOUND
    {
        return Err(OAuthError::Store);
    }
    Ok(())
}

fn delete_if_current_sync(key: &TokenKey, previous: &OAuthTokens) -> Result<bool, OAuthError> {
    let _lock = lock_credential(key)?;
    let Some(record) = load_record_sync(key)? else {
        return Ok(false);
    };
    if &OAuthTokens::try_from(record)? != previous {
        return Ok(false);
    }
    delete_record_sync(key)?;
    Ok(true)
}

#[async_trait]
impl TokenStore for WindowsCredentialTokenStore {
    async fn load(&self, key: &TokenKey) -> Result<Option<OAuthTokens>, OAuthError> {
        let key = key.clone();
        tokio::task::spawn_blocking(move || load_sync(&key))
            .await
            .map_err(|_| OAuthError::Store)?
    }

    async fn save(&self, key: &TokenKey, tokens: OAuthTokens) -> Result<(), OAuthError> {
        let key = key.clone();
        tokio::task::spawn_blocking(move || save_sync(&key, tokens))
            .await
            .map_err(|_| OAuthError::Store)?
    }

    async fn delete(&self, key: &TokenKey) -> Result<(), OAuthError> {
        let key = key.clone();
        tokio::task::spawn_blocking(move || delete_sync(&key))
            .await
            .map_err(|_| OAuthError::Store)?
    }

    async fn resume_grant_identity(&self, key: &TokenKey) -> Result<Option<String>, OAuthError> {
        let key = key.clone();
        tokio::task::spawn_blocking(move || resume_grant_identity_sync(&key))
            .await
            .map_err(|_| OAuthError::Store)?
    }

    async fn save_refreshed(
        &self,
        key: &TokenKey,
        previous: &OAuthTokens,
        tokens: OAuthTokens,
    ) -> Result<(), OAuthError> {
        let key = key.clone();
        let previous = previous.clone();
        tokio::task::spawn_blocking(move || save_refreshed_sync(&key, &previous, tokens))
            .await
            .map_err(|_| OAuthError::Store)?
    }

    async fn delete_if_current(
        &self,
        key: &TokenKey,
        previous: &OAuthTokens,
    ) -> Result<bool, OAuthError> {
        let key = key.clone();
        let previous = previous.clone();
        tokio::task::spawn_blocking(move || delete_if_current_sync(&key, &previous))
            .await
            .map_err(|_| OAuthError::Store)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> TokenKey {
        TokenKey {
            resource: "https://mcp.example.test/endpoint".into(),
            issuer: "https://auth.example.test".into(),
            client_id: "client-123".into(),
        }
    }

    #[test]
    fn target_is_stable_bounded_and_separates_principals() {
        let key = key();
        let target = target_name(&key);
        assert_eq!(
            target,
            "OpenIDE:MCP:OAuth:v1:880bf6da641d62b50eabf7fd075999b31f760b0f24d606fc71f5172528ea66ce"
        );
        assert!(target.starts_with(TARGET_PREFIX));
        assert_eq!(target.len(), TARGET_PREFIX.len() + 64);
        assert!(!target.contains(&key.resource));
        assert!(!target.contains(&key.issuer));
        assert!(!target.contains(&key.client_id));
        let mut changed = key.clone();
        changed.client_id.push('x');
        assert_ne!(target_name(&changed), target);
        let left = TokenKey {
            resource: "a".into(),
            issuer: "bc".into(),
            client_id: "d".into(),
        };
        let right = TokenKey {
            resource: "ab".into(),
            issuer: "c".into(),
            client_id: "d".into(),
        };
        assert_ne!(target_name(&left), target_name(&right));
    }

    #[test]
    fn blob_roundtrips_and_rejects_oversize_or_unknown_version() {
        let tokens = OAuthTokens {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at_unix_seconds: Some(12345),
            scopes: vec!["read".into(), "write".into()],
        };
        let encoded = serialize_tokens(tokens).unwrap();
        let decoded = deserialize_tokens(&encoded.0).unwrap();
        assert_eq!(decoded.access_token, "access");
        assert_eq!(decoded.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(decoded.expires_at_unix_seconds, Some(12345));
        assert_eq!(decoded.scopes, ["read", "write"]);
        let oversized = OAuthTokens {
            access_token: "x".repeat(CRED_MAX_CREDENTIAL_BLOB_SIZE as usize),
            refresh_token: None,
            expires_at_unix_seconds: None,
            scopes: vec![],
        };
        assert!(matches!(
            serialize_tokens(oversized),
            Err(OAuthError::Store)
        ));
        assert!(matches!(deserialize_tokens(br#"{"version":2,"access_token":"a","refresh_token":null,"expires_at_unix_seconds":null,"scopes":[]}"#), Err(OAuthError::Store)));
    }

    #[test]
    fn vault_record_preserves_grant_revision_and_legacy_record_has_no_resume_identity() {
        let tokens = OAuthTokens {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at_unix_seconds: None,
            scopes: vec!["read".into()],
        };
        let revision = "a".repeat(64);
        let record = StoredTokens::new(tokens, Some(revision.clone()));
        let bytes = serialize_record(&record).unwrap();
        let recovered = deserialize_record(&bytes.0).unwrap();
        assert_eq!(recovered.grant_revision.as_deref(), Some(revision.as_str()));
        let legacy = deserialize_record(br#"{"version":1,"access_token":"access","refresh_token":null,"expires_at_unix_seconds":null,"scopes":["read"]}"#).unwrap();
        assert_eq!(legacy.grant_revision, None);
        let malformed = br#"{"version":1,"grant_revision":"bad","access_token":"access","refresh_token":null,"expires_at_unix_seconds":null,"scopes":[]}"#;
        assert!(matches!(
            deserialize_record(malformed),
            Err(OAuthError::Store)
        ));
    }

    #[test]
    fn bearer_hmac_is_keyed_and_changes_when_token_rotates() {
        let key = [7u8; 32];
        let other_key = [8u8; 32];
        let first = hmac_sha256(&key, b"bearer-one");
        assert_eq!(first.len(), 64);
        assert_eq!(first, hmac_sha256(&key, b"bearer-one"));
        assert_ne!(first, hmac_sha256(&key, b"bearer-two"));
        assert_ne!(first, hmac_sha256(&other_key, b"bearer-one"));
        assert!(!first.contains("bearer-one"));
    }

    #[tokio::test]
    async fn vault_grant_survives_store_recreation_and_refresh_but_not_reauthorization() {
        let key = TokenKey {
            resource: format!("https://mcp.example.test/{}", uuid::Uuid::new_v4()),
            issuer: "https://issuer.example.test".into(),
            client_id: "test-client".into(),
        };
        let store = WindowsCredentialTokenStore::new();
        let first = OAuthTokens {
            access_token: "first-access".into(),
            refresh_token: Some("first-refresh".into()),
            expires_at_unix_seconds: Some(100),
            scopes: vec!["read".into()],
        };
        let result = async {
            store.save(&key, first.clone()).await?;
            let before = store
                .resume_grant_identity(&key)
                .await?
                .ok_or(OAuthError::Store)?;
            let reopened = WindowsCredentialTokenStore::new();
            assert_eq!(
                reopened.resume_grant_identity(&key).await?,
                Some(before.clone())
            );
            let refreshed = OAuthTokens {
                access_token: "second-access".into(),
                refresh_token: Some("second-refresh".into()),
                ..first.clone()
            };
            reopened
                .save_refreshed(&key, &first, refreshed.clone())
                .await?;
            assert_eq!(
                store.resume_grant_identity(&key).await?,
                Some(before.clone())
            );
            store.save(&key, refreshed).await?;
            assert_ne!(store.resume_grant_identity(&key).await?, Some(before));
            assert_eq!(
                reopened.save_refreshed(&key, &first, first.clone()).await,
                Err(OAuthError::AuthorizationRequired)
            );
            assert!(!reopened.delete_if_current(&key, &first).await?);
            assert!(store.load(&key).await?.is_some());
            Ok::<(), OAuthError>(())
        }
        .await;
        store.delete(&key).await.unwrap();
        result.unwrap();
    }
}
