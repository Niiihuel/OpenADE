use crate::storage;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    token_sha256: Vec<u8>,
    pub revoked: bool,
    #[serde(default)]
    pub created_at_ms: u64,
    /// The last request it made, written at most once a minute.
    #[serde(default)]
    pub last_seen_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as u64)
}

#[derive(Clone)]
pub struct DeviceStore {
    root: PathBuf,
    /// The list as last read, with the file's modification time: requests
    /// with a token do not each read the file.
    cache: Arc<Mutex<Option<(FileStamp, Vec<Device>)>>>,
}

/// What tells one version of the file from another.
type FileStamp = (SystemTime, u64, u64);

fn stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&metadata);
    #[cfg(not(unix))]
    let inode = 0;
    Some((metadata.modified().ok()?, metadata.len(), inode))
}

impl DeviceStore {
    fn write(&self, devices: &[Device]) -> Result<()> {
        *self.cache.lock().unwrap_or_else(|error| error.into_inner()) = None;
        storage::write_json(&self.root.join("devices.json"), &devices)
    }

    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            cache: Arc::default(),
        }
    }

    pub fn issue(&self, name: &str) -> Result<(Device, String)> {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let device = self.issue_prepared(name, &Uuid::new_v4().to_string(), &token)?;
        Ok((device, token))
    }

    pub fn issue_prepared(&self, name: &str, id: &str, token: &str) -> Result<Device> {
        if name.trim().is_empty() || name.len() > 128 {
            bail!("invalid device name");
        }
        storage::validate_id(id)?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid device token");
        }
        storage::prepare_directory(&self.root)?;
        let _lock = storage::lock_file(&self.root.join("devices.lock"))?;
        let mut devices = self.list()?;
        let digest = Sha256::digest(token.as_bytes()).to_vec();
        if let Some(device) = devices.iter().find(|d| d.id == id) {
            if device.revoked
                || device.name != name.trim()
                || !bool::from(device.token_sha256.as_slice().ct_eq(digest.as_slice()))
            {
                bail!("device identity already exists");
            }
            return Ok(device.clone());
        }
        if devices.iter().filter(|device| !device.revoked).count() >= 128 {
            bail!("device limit reached");
        }
        let device = Device {
            id: id.to_owned(),
            name: name.trim().into(),
            token_sha256: digest,
            revoked: false,
            created_at_ms: now_ms(),
            last_seen_ms: now_ms(),
        };
        devices.push(device.clone());
        self.write(&devices)?;
        Ok(device)
    }

    pub fn list(&self) -> Result<Vec<Device>> {
        let path = self.root.join("devices.json");
        let Some(current) = stamp(&path) else {
            return Ok(Vec::new());
        };
        let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((_, devices)) = cache.as_ref().filter(|(read, _)| *read == current) {
            return Ok(devices.clone());
        }
        let devices: Vec<Device> = storage::read_json(&path)?;
        *cache = Some((current, devices.clone()));
        Ok(devices)
    }

    /// The device still has access.
    pub fn is_active(&self, id: &str) -> bool {
        self.list().is_ok_and(|devices| devices.iter().any(|device| device.id == id && !device.revoked))
    }

    pub fn revoke(&self, id: &str) -> Result<()> {
        let _lock = storage::lock_file(&self.root.join("devices.lock"))?;
        let mut devices = self.list()?;
        let device = devices
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or_else(|| anyhow::anyhow!("device not found"))?;
        device.revoked = true;
        self.write(&devices)
    }

    /// Records that `id` was just seen, unless that was recorded lately.
    pub fn touch(&self, id: &str) -> Result<()> {
        let now = now_ms();
        if self.list()?.iter().any(|device| device.id == id && now.saturating_sub(device.last_seen_ms) < 60_000) {
            return Ok(());
        }
        let _lock = storage::lock_file(&self.root.join("devices.lock"))?;
        let mut devices = self.list()?;
        if let Some(device) = devices.iter_mut().find(|device| device.id == id) {
            device.last_seen_ms = now;
            self.write(&devices)?;
        }
        Ok(())
    }

    pub fn authenticate_device(&self, token: &str) -> Option<Device> {
        if token.len() != 64 {
            return None;
        }
        let digest = Sha256::digest(token.as_bytes());
        let Ok(devices) = self.list() else {
            return None;
        };
        // Reload on each request: revocation is effective without restart.
        let mut valid = None;
        for device in devices {
            let matched = device.token_sha256.len() == 32
                && bool::from(device.token_sha256.as_slice().ct_eq(digest.as_slice()));
            if matched && !device.revoked {
                valid = Some(device);
            }
        }
        valid
    }
}
