//! OAuth grants of MCP servers kept in private files, for a host without a
//! credential vault of its own. One file per grant under a directory only the
//! user can enter; each is replaced whole, and every change holds one lock, so
//! two processes never interleave a refresh.

use crate::mcp_oauth::{grant_identity_digest, OAuthError, OAuthTokens, TokenKey, TokenStore};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

const MAX_GRANT_BYTES: u64 = 256 * 1024;

pub struct FileTokenStore {
    directory: PathBuf,
}

/// What one grant's file holds. The key is kept beside the tokens: a file
/// under another grant's name is not that grant.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    version: u32,
    resource: String,
    issuer: String,
    client_id: String,
    /// Changes with each authorization, never with a refresh.
    revision: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at_unix_seconds: Option<u64>,
    scopes: Vec<String>,
}

impl Grant {
    fn tokens(&self) -> OAuthTokens {
        OAuthTokens {
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at_unix_seconds: self.expires_at_unix_seconds,
            scopes: self.scopes.clone(),
        }
    }
}

/// Held for the length of one change.
struct Locked(File);

impl Drop for Locked {
    fn drop(&mut self) {
        // SAFETY: the descriptor is this open file's own.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl FileTokenStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self { directory: directory.into() }
    }

    fn path(&self, key: &TokenKey) -> PathBuf {
        let mut hasher = Sha256::new();
        for value in [&key.resource, &key.issuer, &key.client_id] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        let name: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
        self.directory.join(format!("{name}.json"))
    }

    /// The directory, private to the user, and the lock every change holds.
    fn lock(&self) -> Result<Locked, OAuthError> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.directory).map_err(|_| OAuthError::Store)?;
        // One that was there before may have been left open to others.
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700)).map_err(|_| OAuthError::Store)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.directory.join(".lock"))
            .map_err(|_| OAuthError::Store)?;
        // SAFETY: the descriptor is this open file's own.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(OAuthError::Store);
        }
        Ok(Locked(file))
    }

    fn read(&self, key: &TokenKey) -> Result<Option<Grant>, OAuthError> {
        let path = self.path(key);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(OAuthError::Store),
        };
        // Only a plain file of this user's alone is a grant.
        if !metadata.is_file() || metadata.len() > MAX_GRANT_BYTES || metadata.permissions().mode() & 0o077 != 0 {
            return Err(OAuthError::Store);
        }
        let grant: Grant = serde_json::from_slice(&fs::read(&path).map_err(|_| OAuthError::Store)?).map_err(|_| OAuthError::Store)?;
        let same = grant.version == 1 && grant.resource == key.resource && grant.issuer == key.issuer && grant.client_id == key.client_id;
        if same {
            Ok(Some(grant))
        } else {
            Err(OAuthError::Store)
        }
    }

    fn write(&self, key: &TokenKey, revision: String, tokens: OAuthTokens) -> Result<(), OAuthError> {
        let grant = Grant {
            version: 1,
            resource: key.resource.clone(),
            issuer: key.issuer.clone(),
            client_id: key.client_id.clone(),
            revision,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at_unix_seconds: tokens.expires_at_unix_seconds,
            scopes: tokens.scopes,
        };
        let bytes = serde_json::to_vec(&grant).map_err(|_| OAuthError::Store)?;
        let path = self.path(key);
        let partial = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
        let written = (|| {
            let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&partial)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&partial, &path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&partial);
        }
        written.map_err(|_| OAuthError::Store)
    }
}

#[async_trait]
impl TokenStore for FileTokenStore {
    async fn load(&self, key: &TokenKey) -> Result<Option<OAuthTokens>, OAuthError> {
        let _lock = self.lock()?;
        Ok(self.read(key)?.map(|grant| grant.tokens()))
    }

    async fn save(&self, key: &TokenKey, tokens: OAuthTokens) -> Result<(), OAuthError> {
        let _lock = self.lock()?;
        self.write(key, uuid::Uuid::new_v4().simple().to_string(), tokens)
    }

    async fn delete(&self, key: &TokenKey) -> Result<(), OAuthError> {
        let _lock = self.lock()?;
        match fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(OAuthError::Store),
        }
    }

    async fn resume_grant_identity(&self, key: &TokenKey) -> Result<Option<String>, OAuthError> {
        let _lock = self.lock()?;
        Ok(self.read(key)?.map(|grant| grant_identity_digest(key, &grant.revision, &grant.scopes)))
    }

    async fn save_refreshed(&self, key: &TokenKey, previous: &OAuthTokens, tokens: OAuthTokens) -> Result<(), OAuthError> {
        let _lock = self.lock()?;
        let grant = self.read(key)?.ok_or(OAuthError::AuthorizationRequired)?;
        if &grant.tokens() != previous {
            return Err(OAuthError::AuthorizationRequired);
        }
        self.write(key, grant.revision, tokens)
    }

    async fn delete_if_current(&self, key: &TokenKey, previous: &OAuthTokens) -> Result<bool, OAuthError> {
        let _lock = self.lock()?;
        if self.read(key)?.is_some_and(|grant| &grant.tokens() == previous) {
            fs::remove_file(self.path(key)).map_err(|_| OAuthError::Store)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(resource: &str) -> TokenKey {
        TokenKey { resource: resource.into(), issuer: "https://auth.example.test".into(), client_id: "cliente".into() }
    }

    fn tokens(access: &str) -> OAuthTokens {
        OAuthTokens {
            access_token: access.into(),
            refresh_token: Some(format!("{access}-refresh")),
            expires_at_unix_seconds: Some(1_900_000_000),
            scopes: vec!["read".into()],
        }
    }

    #[tokio::test]
    async fn a_grant_is_kept_private_survives_a_new_store_and_is_refreshed_only_if_current() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("mcp-oauth");
        let store = FileTokenStore::new(&directory);
        let (one, other) = (key("https://mcp.example.test/"), key("https://otro.example.test/"));
        assert!(store.load(&one).await.unwrap().is_none());
        store.save(&one, tokens("a")).await.unwrap();
        // Only the user can enter the directory or read the grant.
        assert_eq!(fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(store.path(&one)).unwrap().permissions().mode() & 0o777, 0o600);
        // Another store on the same directory, as after a restart, has it; another key does not.
        let again = FileTokenStore::new(&directory);
        assert!(again.load(&one).await.unwrap() == Some(tokens("a")));
        assert!(again.load(&other).await.unwrap().is_none());

        // A refresh replaces only the tokens it started from, and keeps the grant's identity.
        let identity = again.resume_grant_identity(&one).await.unwrap().unwrap();
        again.save_refreshed(&one, &tokens("a"), tokens("b")).await.unwrap();
        assert!(matches!(again.save_refreshed(&one, &tokens("a"), tokens("c")).await, Err(OAuthError::AuthorizationRequired)));
        assert!(store.load(&one).await.unwrap() == Some(tokens("b")));
        assert_eq!(store.resume_grant_identity(&one).await.unwrap().unwrap(), identity);
        // A new authorization is another grant.
        store.save(&one, tokens("d")).await.unwrap();
        assert_ne!(store.resume_grant_identity(&one).await.unwrap().unwrap(), identity);

        // A rejected refresh does not erase a newer authorization.
        assert!(!store.delete_if_current(&one, &tokens("b")).await.unwrap());
        assert!(store.delete_if_current(&one, &tokens("d")).await.unwrap());
        assert!(store.load(&one).await.unwrap().is_none());
        store.delete(&one).await.unwrap();
    }

    #[tokio::test]
    async fn a_file_others_can_read_or_one_that_names_another_grant_is_not_trusted() {
        let root = tempfile::tempdir().unwrap();
        let store = FileTokenStore::new(root.path().join("mcp-oauth"));
        let (one, other) = (key("https://mcp.example.test/"), key("https://otro.example.test/"));
        store.save(&one, tokens("a")).await.unwrap();
        // Copied under another grant's name: it still says whose it is.
        fs::copy(store.path(&one), store.path(&other)).unwrap();
        assert!(matches!(store.load(&other).await, Err(OAuthError::Store)));
        fs::set_permissions(store.path(&one), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(store.load(&one).await, Err(OAuthError::Store)));
        fs::write(store.path(&one), b"{").unwrap();
        fs::set_permissions(store.path(&one), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(store.load(&one).await, Err(OAuthError::Store)));
    }
}
