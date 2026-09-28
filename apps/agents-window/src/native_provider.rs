//! Persistent connection choice for the native harness.
//!
//! The models.dev catalog supplies display metadata, not trusted connection
//! targets. Only adapters listed here may use provider credentials. A custom
//! endpoint is an explicit user choice and uses the legacy OpenIDE_API_KEY.

use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "native-provider.json";
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const DEFAULT_MODEL: &str = "gpt-4.1";
const DEFAULT_CUSTOM_ENDPOINT: &str = "https://api.openai.com/v1";
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Adapter {
    pub id: &'static str,
    pub name: &'static str,
    pub endpoint: &'static str,
    pub key_env: &'static str,
}

const ADAPTERS: [Adapter; 4] = [
    Adapter {
        id: "openai",
        name: "OpenAI",
        endpoint: "https://api.openai.com/v1",
        key_env: "OPENAI_API_KEY",
    },
    Adapter {
        id: "openrouter",
        name: "OpenRouter",
        endpoint: "https://openrouter.ai/api/v1",
        key_env: "OPENROUTER_API_KEY",
    },
    Adapter {
        id: "deepseek",
        name: "DeepSeek",
        endpoint: "https://api.deepseek.com",
        key_env: "DEEPSEEK_API_KEY",
    },
    Adapter {
        id: "custom",
        name: "Personalizado",
        endpoint: DEFAULT_CUSTOM_ENDPOINT,
        key_env: "OPENIDE_API_KEY",
    },
];

pub(crate) fn adapter(provider_id: &str) -> Option<Adapter> {
    ADAPTERS
        .iter()
        .copied()
        .find(|entry| entry.id == provider_id)
}

/// Reviewed OpenAI IDs for function tools with streaming Chat Completions.
/// A newly published catalog entry remains visible until its wire contract is
/// checked; users can still opt into a custom compatible gateway explicitly.
pub(crate) fn openai_native_compatible_model(model_id: &str) -> bool {
    matches!(
        model_id,
        "gpt-4.1"
            | "gpt-4.1-mini"
            | "gpt-4o"
            | "gpt-4o-mini"
            | "gpt-4o-2024-08-06"
            | "gpt-4o-2024-11-20"
            | "gpt-5"
            | "gpt-5-mini"
            | "gpt-5-nano"
            | "gpt-5.1"
            | "gpt-5.2"
            | "gpt-5.3-codex"
            | "gpt-5.4"
            | "gpt-5.4-mini"
            | "gpt-5.4-nano"
            | "gpt-5.5"
            | "gpt-5.6"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "o3"
            | "gpt-6-sol"
            | "gpt-6-luna"
    )
}

/// DeepSeek's currently documented canonical Chat Completions IDs. Older V4
/// aliases may be redirected today but can disappear independently of the catalog.
pub(crate) fn deepseek_native_compatible_model(model_id: &str) -> bool {
    matches!(model_id, "deepseek-flash" | "deepseek-v4-pro")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct NativeProviderSettings {
    pub provider_id: String,
    pub model_id: String,
    pub custom_endpoint: String,
}

impl Default for NativeProviderSettings {
    fn default() -> Self {
        Self {
            provider_id: "custom".to_owned(),
            model_id: DEFAULT_MODEL.to_owned(),
            custom_endpoint: DEFAULT_CUSTOM_ENDPOINT.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeConnection {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model_id: String,
}

impl NativeProviderSettings {
    /// A persisted choice wins. Without one, import the original native harness
    /// environment variables; migration is written only when `save` is called.
    pub fn load(root: &Path) -> Self {
        let fallback = || {
            Self::from_legacy(
                std::env::var("OPENIDE_MODEL").ok(),
                std::env::var("OPENIDE_API_BASE").ok(),
            )
        };
        let path = root.join(SETTINGS_FILE);
        if !fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_SETTINGS_BYTES) {
            return fallback();
        }
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(fallback)
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let normalized = Self {
            provider_id: self.provider_id.trim().to_owned(),
            model_id: self.model_id.trim().to_owned(),
            custom_endpoint: self.custom_endpoint.trim().to_owned(),
        };
        normalized.validated_endpoint()?;
        fs::create_dir_all(root)
            .map_err(|error| format!("No se pudo crear la carpeta de configuración: {error}"))?;
        let payload = serde_json::to_vec_pretty(&normalized)
            .map_err(|error| format!("No se pudo serializar la configuración nativa: {error}"))?;
        let temp = root.join(format!(
            ".native-provider-{}-{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&payload)?;
            file.sync_all()?;
            drop(file);
            replace_settings_atomically(&temp, &root.join(SETTINGS_FILE))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|error| format!("No se pudo guardar la configuración nativa: {error}"))
    }

    pub fn resolve(&self) -> Result<RuntimeConnection, String> {
        self.resolve_with_key(|name| std::env::var(name).ok())
    }

    fn resolve_with_key(
        &self,
        read_key: impl FnOnce(&str) -> Option<String>,
    ) -> Result<RuntimeConnection, String> {
        let endpoint = self.validated_endpoint()?;
        let selected = adapter(self.provider_id.trim()).expect("validated provider");
        Ok(RuntimeConnection {
            endpoint,
            api_key: read_key(selected.key_env)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.trim().is_empty()),
            model_id: self.model_id.trim().to_owned(),
        })
    }

    pub fn key_available(&self) -> bool {
        self.resolve()
            .is_ok_and(|connection| connection.api_key.is_some())
    }

    fn from_legacy(model: Option<String>, endpoint: Option<String>) -> Self {
        let mut settings = Self::default();
        if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
            settings.model_id = model.trim().to_owned();
        }
        if let Some(endpoint) = endpoint.filter(|value| !value.trim().is_empty()) {
            settings.custom_endpoint = endpoint.trim().to_owned();
        }
        settings
    }

    fn validated_endpoint(&self) -> Result<String, String> {
        if self.model_id.trim().is_empty() {
            return Err("Selecciona un modelo para el agente nativo".to_owned());
        }
        let provider_id = self.provider_id.trim();
        let selected = adapter(provider_id)
            .ok_or_else(|| format!("Proveedor nativo no compatible: {provider_id}"))?;
        if provider_id == "openai" && !openai_native_compatible_model(self.model_id.trim()) {
            return Err(
                "Este modelo de OpenAI aún no está verificado para herramientas y streaming de Chat Completions"
                    .to_owned(),
            );
        }
        if provider_id == "deepseek" && !deepseek_native_compatible_model(self.model_id.trim()) {
            return Err(
                "Este modelo de DeepSeek no es un ID canónico verificado para el harness nativo"
                    .to_owned(),
            );
        }
        if provider_id == "custom" {
            validate_custom_endpoint(&self.custom_endpoint)
        } else {
            Ok(selected.endpoint.to_owned())
        }
    }
}

#[cfg(not(windows))]
fn replace_settings_atomically(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_settings_atomically(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    #[link(name = "Kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced_file: *const u16,
            replacement_file: *const u16,
            backup_file: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    if !destination.exists() {
        return fs::rename(temporary, destination);
    }
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let temporary_wide: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            temporary_wide.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if replaced != 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if !destination.exists() {
            fs::rename(temporary, destination)
        } else {
            Err(error)
        }
    }
}

fn validate_custom_endpoint(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 2048 || value.chars().any(char::is_whitespace) {
        return Err("Ingresa una URL válida para el endpoint personalizado".to_owned());
    }
    let url = reqwest::Url::parse(value)
        .map_err(|_| "Ingresa una URL válida para el endpoint personalizado".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("El endpoint personalizado debe ser una URL HTTP(S) sin credenciales, búsqueda ni fragmento".to_owned());
    }
    if url.scheme() == "http" {
        let host = url.host_str().unwrap_or_default();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if !loopback {
            return Err(
                "Usa HTTPS para endpoints remotos; HTTP sólo se permite en localhost".to_owned(),
            );
        }
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_allowlist_keeps_credentials_provider_specific() {
        assert_eq!(ADAPTERS.len(), 4);
        assert_eq!(adapter("openai").unwrap().key_env, "OPENAI_API_KEY");
        assert_eq!(
            adapter("openrouter").unwrap().endpoint,
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            adapter("deepseek").unwrap().endpoint,
            "https://api.deepseek.com"
        );
        assert_eq!(adapter("custom").unwrap().key_env, "OPENIDE_API_KEY");
        assert!(adapter("unknown").is_none());
    }

    #[test]
    fn legacy_migration_keeps_original_defaults_and_overrides() {
        assert_eq!(
            NativeProviderSettings::from_legacy(None, None),
            NativeProviderSettings::default()
        );
        let migrated = NativeProviderSettings::from_legacy(
            Some(" my-model ".to_owned()),
            Some(" http://localhost:1234/v1 ".to_owned()),
        );
        assert_eq!(migrated.provider_id, "custom");
        assert_eq!(migrated.model_id, "my-model");
        assert_eq!(migrated.custom_endpoint, "http://localhost:1234/v1");
    }

    #[test]
    fn settings_round_trip_without_secrets() {
        let root = tempfile::tempdir().unwrap();
        let settings = NativeProviderSettings {
            provider_id: "openrouter".to_owned(),
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            custom_endpoint: DEFAULT_CUSTOM_ENDPOINT.to_owned(),
        };
        settings.save(root.path()).unwrap();
        assert_eq!(NativeProviderSettings::load(root.path()), settings);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(root.path().join(SETTINGS_FILE)).unwrap()).unwrap();
        let fields = json.as_object().unwrap();
        assert_eq!(fields.len(), 3);
        assert!(fields.contains_key("provider_id"));
        assert!(fields.contains_key("model_id"));
        assert!(fields.contains_key("custom_endpoint"));
        assert!(!fields.contains_key("api_key"));
    }

    #[test]
    fn save_replaces_existing_choice() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = NativeProviderSettings::default();
        settings.save(root.path()).unwrap();
        settings.provider_id = "deepseek".to_owned();
        settings.model_id = "deepseek-flash".to_owned();
        settings.save(root.path()).unwrap();
        assert_eq!(NativeProviderSettings::load(root.path()), settings);
    }

    #[test]
    fn resolve_uses_local_allowlist_instead_of_custom_endpoint_for_known_provider() {
        let settings = NativeProviderSettings {
            provider_id: "openai".to_owned(),
            model_id: "gpt-4.1".to_owned(),
            custom_endpoint: "https://untrusted.example/v1".to_owned(),
        };
        let connection = settings.resolve().unwrap();
        assert_eq!(connection.endpoint, "https://api.openai.com/v1");
        assert_eq!(connection.model_id, "gpt-4.1");
    }

    #[test]
    fn resolve_reads_only_the_selected_provider_key() {
        for (provider_id, key_env) in [
            ("openai", "OPENAI_API_KEY"),
            ("openrouter", "OPENROUTER_API_KEY"),
            ("deepseek", "DEEPSEEK_API_KEY"),
            ("custom", "OPENIDE_API_KEY"),
        ] {
            let settings = NativeProviderSettings {
                provider_id: provider_id.to_owned(),
                model_id: if provider_id == "deepseek" {
                    "deepseek-flash".to_owned()
                } else {
                    DEFAULT_MODEL.to_owned()
                },
                ..NativeProviderSettings::default()
            };
            let connection = settings
                .resolve_with_key(|name| {
                    assert_eq!(name, key_env);
                    Some("  secret  ".to_owned())
                })
                .unwrap();
            assert_eq!(connection.api_key.as_deref(), Some("secret"));
        }
    }

    #[test]
    fn custom_endpoint_accepts_secure_remote_and_loopback_http() {
        for value in [
            "https://example.com/v1",
            "http://localhost:1234/v1",
            "http://127.0.0.1:11434/v1",
            "http://[::1]:1234/v1",
        ] {
            assert!(validate_custom_endpoint(value).is_ok(), "{value}");
        }
    }

    #[test]
    fn custom_endpoint_rejects_unsafe_or_ambiguous_urls() {
        for value in [
            "",
            "not-a-url",
            "ftp://example.com/v1",
            "http://example.com/v1",
            "http://192.168.1.10:1234/v1",
            "https://user:password@example.com/v1",
            "https://example.com/v1?token=secret",
            "https://example.com/v1#fragment",
            "https://example.com/a b",
        ] {
            assert!(validate_custom_endpoint(value).is_err(), "{value}");
        }
    }

    #[test]
    fn empty_model_and_unknown_provider_are_rejected() {
        let mut settings = NativeProviderSettings {
            model_id: "  ".to_owned(),
            ..NativeProviderSettings::default()
        };
        assert!(settings.resolve().is_err());
        settings.model_id = "gpt-4.1".to_owned();
        settings.provider_id = "unlisted".to_owned();
        assert!(settings.resolve().is_err());
    }

    #[test]
    fn openai_models_need_explicit_chat_stream_review() {
        for model in [
            "gpt-5.4-pro",
            "gpt-5.4-pro-2026-03-05",
            "o3-pro",
            "gpt-realtime-2.1",
            "gpt-future-unknown",
        ] {
            assert!(!openai_native_compatible_model(model));
            let settings = NativeProviderSettings {
                provider_id: "openai".into(),
                model_id: model.into(),
                ..NativeProviderSettings::default()
            };
            assert!(settings.resolve().is_err());
        }
        assert!(openai_native_compatible_model("gpt-5.4"));
    }

    #[test]
    fn deepseek_aliases_are_visible_but_not_native_connections() {
        assert!(deepseek_native_compatible_model("deepseek-flash"));
        assert!(deepseek_native_compatible_model("deepseek-v4-pro"));
        assert!(!deepseek_native_compatible_model("deepseek-v4-flash"));
    }
}
