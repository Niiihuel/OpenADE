//! Models.dev metadata for the native provider and model settings.
//!
//! This catalog describes models; it does not authorize an API endpoint or
//! imply that the native harness implements a provider's wire protocol. Call
//! `refresh` on a worker thread, then give the resulting snapshot to the UI.

use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, ETAG, IF_NONE_MATCH};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const CATALOG_URL: &str = "https://models.dev/api.json";
const CACHE_FILE: &str = "models-dev-catalog.json";
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 24 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_PROVIDERS_FOR_DROP_GUARD: usize = 10;
const MIN_MODELS_FOR_DROP_GUARD: usize = 100;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub providers: BTreeMap<String, ProviderInfo>,
    pub fetched_at_ms: u64,
    #[serde(default)]
    pub etag: Option<String>,
}

impl CatalogSnapshot {
    pub fn is_stale(&self, ttl: Duration) -> bool {
        now_ms().saturating_sub(self.fetched_at_ms) >= ttl.as_millis() as u64
    }

    /// Discovery metadata only. A caller must separately resolve an adapter,
    /// credential source, and reviewed or explicit execution route.
    pub fn model(&self, provider_id: &str, model_id: &str) -> Option<&ModelInfo> {
        self.providers.get(provider_id)?.models.get(model_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub npm: Option<String>,
    /// Metadata only. Never use this URL as an unreviewed destination for a key.
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelInfo>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default)]
    pub structured_output: bool,
    #[serde(default)]
    pub temperature: bool,
    #[serde(default)]
    pub open_weights: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub last_updated: Option<String>,
    #[serde(default)]
    pub limit: Option<ModelLimit>,
    #[serde(default)]
    pub modalities: Option<ModelModalities>,
    #[serde(default)]
    pub cost: Option<ModelCost>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelLimit {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub input: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Reads the last complete snapshot. Missing cache is normal; a corrupt cache
/// is an error so callers can report it and still try a network refresh.
pub fn load_cache(root: &Path) -> Result<Option<CatalogSnapshot>, String> {
    let path = cache_path(root);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot open catalog cache: {error}")),
    };
    if file
        .metadata()
        .map_err(|error| format!("cannot inspect catalog cache: {error}"))?
        .len()
        > MAX_CACHE_BYTES
    {
        return Err("catalog cache exceeds the size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read catalog cache: {error}"))?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Err("catalog cache exceeds the size limit".into());
    }
    let snapshot: CatalogSnapshot = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid catalog cache: {error}"))?;
    validate_snapshot(&snapshot)?;
    Ok(Some(snapshot))
}

/// Fetches provider and model metadata with an ETag conditional request.
/// Network and disk errors leave the previous cache untouched. The caller may
/// keep using `previous` when this returns an error.
pub fn refresh(root: &Path, previous: Option<&CatalogSnapshot>) -> Result<CatalogSnapshot, String> {
    refresh_from_url(root, previous, CATALOG_URL)
}

fn refresh_from_url(
    root: &Path,
    previous: Option<&CatalogSnapshot>,
    url: &str,
) -> Result<CatalogSnapshot, String> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| format!("cannot create catalog HTTP client: {error}"))?;
    let mut request = client.get(url).header("Accept", "application/json");
    if let Some(etag) = previous
        .and_then(|snapshot| snapshot.etag.as_deref())
        .and_then(|etag| HeaderValue::from_str(etag).ok())
    {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let response = request
        .send()
        .map_err(|error| format!("catalog request failed: {error}"))?;
    if response.status() == StatusCode::NOT_MODIFIED {
        let mut snapshot = previous
            .cloned()
            .ok_or("catalog server returned 304 without a previous snapshot")?;
        validate_snapshot(&snapshot)?;
        snapshot.fetched_at_ms = now_ms();
        write_cache(root, &snapshot)?;
        return Ok(snapshot);
    }
    if !response.status().is_success() {
        return Err(format!(
            "catalog server returned HTTP {}",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err("catalog response exceeds the size limit".into());
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read catalog response: {error}"))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("catalog response exceeds the size limit".into());
    }
    let snapshot = parse_catalog(&bytes, now_ms(), etag)?;
    if let Some(previous) = previous {
        reject_sudden_catalog_drop(previous, &snapshot)?;
    }
    write_cache(root, &snapshot)?;
    Ok(snapshot)
}

/// A successful HTTP response can still be a partial catalog. Small fixtures
/// and initial loads are exempt; a large established cache is preserved when
/// the replacement suddenly loses more than half its providers or models.
fn reject_sudden_catalog_drop(
    previous: &CatalogSnapshot,
    replacement: &CatalogSnapshot,
) -> Result<(), String> {
    let previous_providers = previous.providers.len();
    let replacement_providers = replacement.providers.len();
    if previous_providers >= MIN_PROVIDERS_FOR_DROP_GUARD
        && replacement_providers.saturating_mul(2) < previous_providers
    {
        return Err(format!(
            "catalog refresh rejected: providers dropped from {previous_providers} to {replacement_providers}"
        ));
    }

    let previous_models: usize = previous
        .providers
        .values()
        .map(|provider| provider.models.len())
        .sum();
    let replacement_models: usize = replacement
        .providers
        .values()
        .map(|provider| provider.models.len())
        .sum();
    if previous_models >= MIN_MODELS_FOR_DROP_GUARD
        && replacement_models.saturating_mul(2) < previous_models
    {
        return Err(format!(
            "catalog refresh rejected: models dropped from {previous_models} to {replacement_models}"
        ));
    }
    if previous_providers >= MIN_PROVIDERS_FOR_DROP_GUARD {
        for provider_id in ["openai", "openrouter", "deepseek"] {
            if previous.providers.contains_key(provider_id)
                && !replacement.providers.contains_key(provider_id)
            {
                return Err(format!(
                    "catalog refresh rejected: native provider {provider_id} disappeared"
                ));
            }
        }
    }
    Ok(())
}

fn parse_catalog(
    bytes: &[u8],
    fetched_at_ms: u64,
    etag: Option<String>,
) -> Result<CatalogSnapshot, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid catalog JSON: {error}"))?;
    let raw_providers = value
        .as_object()
        .ok_or("catalog root is not a provider map")?;
    let mut providers = BTreeMap::new();
    for (id, raw_provider) in raw_providers {
        let Some(provider) = parse_provider(id, raw_provider) else {
            continue;
        };
        providers.insert(id.clone(), provider);
    }
    let snapshot = CatalogSnapshot {
        providers,
        fetched_at_ms,
        etag,
    };
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn parse_provider(id: &str, value: &Value) -> Option<ProviderInfo> {
    let object = value.as_object()?;
    let raw_models = object.get("models")?.as_object()?;
    let models = raw_models
        .iter()
        .filter_map(|(model_id, value)| {
            parse_model(model_id, value).map(|model| (model_id.clone(), model))
        })
        .collect();
    Some(ProviderInfo {
        id: id.to_owned(),
        name: string_or(object, "name", id),
        npm: optional_string(object, "npm"),
        api: optional_string(object, "api"),
        env: string_list(object, "env"),
        doc: optional_string(object, "doc"),
        models,
        extra: unknown_fields(
            object,
            &["id", "name", "npm", "api", "env", "doc", "models"],
        ),
    })
}

fn parse_model(id: &str, value: &Value) -> Option<ModelInfo> {
    let object = value.as_object()?;
    Some(ModelInfo {
        id: id.to_owned(),
        name: string_or(object, "name", id),
        description: optional_string(object, "description"),
        family: optional_string(object, "family"),
        tool_call: boolean(object, "tool_call"),
        reasoning: boolean(object, "reasoning"),
        attachment: boolean(object, "attachment"),
        structured_output: boolean(object, "structured_output"),
        temperature: boolean(object, "temperature"),
        open_weights: boolean(object, "open_weights"),
        status: optional_string(object, "status"),
        release_date: optional_string(object, "release_date"),
        last_updated: optional_string(object, "last_updated"),
        limit: object.get("limit").and_then(parse_limit),
        modalities: object.get("modalities").and_then(parse_modalities),
        cost: object.get("cost").and_then(parse_cost),
        extra: unknown_fields(
            object,
            &[
                "id",
                "name",
                "description",
                "family",
                "tool_call",
                "reasoning",
                "attachment",
                "structured_output",
                "temperature",
                "open_weights",
                "status",
                "release_date",
                "last_updated",
                "limit",
                "modalities",
                "cost",
            ],
        ),
    })
}

fn parse_limit(value: &Value) -> Option<ModelLimit> {
    let object = value.as_object()?;
    Some(ModelLimit {
        context: object.get("context").and_then(Value::as_u64),
        input: object.get("input").and_then(Value::as_u64),
        output: object.get("output").and_then(Value::as_u64),
        extra: unknown_fields(object, &["context", "input", "output"]),
    })
}

fn parse_modalities(value: &Value) -> Option<ModelModalities> {
    let object = value.as_object()?;
    Some(ModelModalities {
        input: string_list(object, "input"),
        output: string_list(object, "output"),
        extra: unknown_fields(object, &["input", "output"]),
    })
}

fn parse_cost(value: &Value) -> Option<ModelCost> {
    let object = value.as_object()?;
    Some(ModelCost {
        input: object.get("input").and_then(Value::as_f64),
        output: object.get("output").and_then(Value::as_f64),
        cache_read: object.get("cache_read").and_then(Value::as_f64),
        cache_write: object.get("cache_write").and_then(Value::as_f64),
        extra: unknown_fields(object, &["input", "output", "cache_read", "cache_write"]),
    })
}

fn string_or(object: &Map<String, Value>, key: &str, fallback: &str) -> String {
    optional_string(object, key).unwrap_or_else(|| fallback.to_owned())
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn boolean(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn string_list(object: &Map<String, Value>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn unknown_fields(object: &Map<String, Value>, known: &[&str]) -> BTreeMap<String, Value> {
    object
        .iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn validate_snapshot(snapshot: &CatalogSnapshot) -> Result<(), String> {
    if snapshot.providers.is_empty()
        || !snapshot
            .providers
            .values()
            .any(|provider| !provider.models.is_empty())
    {
        return Err("catalog contains no providers with models".into());
    }
    Ok(())
}

fn cache_path(root: &Path) -> PathBuf {
    root.join(CACHE_FILE)
}

fn write_cache(root: &Path, snapshot: &CatalogSnapshot) -> Result<(), String> {
    let bytes = serde_json::to_vec(snapshot)
        .map_err(|error| format!("cannot encode catalog cache: {error}"))?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Err("catalog cache exceeds the size limit".into());
    }
    fs::create_dir_all(root)
        .map_err(|error| format!("cannot create catalog cache directory: {error}"))?;
    let temporary = root.join(format!(".{CACHE_FILE}.{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_cache_atomically(&temporary, &cache_path(root))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result.map_err(|error| format!("cannot update catalog cache: {error}"))
}

#[cfg(not(windows))]
fn replace_cache_atomically(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_cache_atomically(temporary: &Path, destination: &Path) -> std::io::Result<()> {
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    const FIXTURE: &str = r#"{
      "example": {
        "id": "example", "name": "Example AI", "npm": "@ai-sdk/openai-compatible",
        "api": "https://api.example.test/v1", "env": ["EXAMPLE_API_KEY"],
        "doc": "https://example.test/docs", "new_provider_field": 42,
        "models": {
          "model-one": {
            "id": "model-one", "name": "Model One", "tool_call": true,
            "reasoning": true, "status": "beta", "release_date": "2026-01-01",
            "last_updated": "2026-02-01", "limit": {"context": 100000, "output": 8000},
            "modalities": {"input": ["text", "image"], "output": ["text"]},
            "cost": {"input": 1.25, "output": 4.5, "cache_read": 0.1},
            "new_model_field": {"supported": true}
          },
          "model-two": {"name": "Model Two", "tool_call": "future-value", "cost": null}
        }
      }
    }"#;

    #[test]
    fn parses_capabilities_and_preserves_unknown_fields() {
        let snapshot = parse_catalog(FIXTURE.as_bytes(), 123, Some("\"etag\"".into())).unwrap();
        let provider = &snapshot.providers["example"];
        assert_eq!(provider.id, "example");
        assert_eq!(provider.env, ["EXAMPLE_API_KEY"]);
        assert_eq!(provider.extra["new_provider_field"], json!(42));
        let model = &provider.models["model-one"];
        assert!(model.tool_call);
        assert!(model.reasoning);
        assert_eq!(model.limit.as_ref().unwrap().context, Some(100_000));
        assert_eq!(model.modalities.as_ref().unwrap().input, ["text", "image"]);
        assert_eq!(model.cost.as_ref().unwrap().cache_read, Some(0.1));
        assert_eq!(model.extra["new_model_field"], json!({"supported": true}));
        assert!(!provider.models["model-two"].tool_call);
        assert_eq!(provider.models["model-two"].cost, None);
        assert_eq!(snapshot.model("example", "model-one"), Some(model));
        assert!(snapshot.model("example", "absent").is_none());
        assert!(snapshot.model("absent", "model-one").is_none());
    }

    #[test]
    fn rejects_empty_or_unrelated_payloads() {
        assert!(parse_catalog(b"{}", 0, None).is_err());
        assert!(parse_catalog(br#"{"error":"unavailable"}"#, 0, None).is_err());
        assert!(parse_catalog(b"[]", 0, None).is_err());
    }

    #[test]
    fn cache_replacement_keeps_a_complete_snapshot_on_windows_too() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(load_cache(root.path()).unwrap(), None);
        let first = parse_catalog(FIXTURE.as_bytes(), 1, Some("\"v1\"".into())).unwrap();
        write_cache(root.path(), &first).unwrap();
        assert_eq!(load_cache(root.path()).unwrap(), Some(first.clone()));

        let mut next = first.clone();
        next.fetched_at_ms = 2;
        next.etag = Some("\"v2\"".into());
        write_cache(root.path(), &next).unwrap();
        assert_eq!(load_cache(root.path()).unwrap(), Some(next));

        fs::write(cache_path(root.path()), b"bad JSON").unwrap();
        assert!(load_cache(root.path()).is_err());
    }

    #[test]
    fn conditional_refresh_reuses_snapshot_on_not_modified() {
        let root = tempfile::tempdir().unwrap();
        let previous = parse_catalog(FIXTURE.as_bytes(), 1, Some("\"known\"".into())).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut saw_etag = false;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if line.eq_ignore_ascii_case("if-none-match: \"known\"\r\n") {
                    saw_etag = true;
                }
            }
            assert!(saw_etag);
            socket
                .write_all(b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let refreshed = refresh_from_url(
            root.path(),
            Some(&previous),
            &format!("http://{address}/api.json"),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(refreshed.providers, previous.providers);
        assert_eq!(refreshed.etag, previous.etag);
        assert!(refreshed.fetched_at_ms > previous.fetched_at_ms);
        assert_eq!(load_cache(root.path()).unwrap(), Some(refreshed));
    }

    fn catalog_with_counts(provider_count: usize, models_per_provider: usize) -> Vec<u8> {
        let mut providers = Map::new();
        for provider_index in 0..provider_count {
            let mut models = Map::new();
            for model_index in 0..models_per_provider {
                models.insert(
                    format!("model-{model_index}"),
                    json!({ "name": format!("Model {model_index}") }),
                );
            }
            providers.insert(
                format!("provider-{provider_index}"),
                json!({ "name": format!("Provider {provider_index}"), "models": models }),
            );
        }
        serde_json::to_vec(&Value::Object(providers)).unwrap()
    }

    #[test]
    fn partial_http_catalog_does_not_replace_large_cache() {
        for (provider_count, models_per_provider, expected_drop) in
            [(20, 1, "providers dropped"), (1, 120, "models dropped")]
        {
            let root = tempfile::tempdir().unwrap();
            let previous = parse_catalog(
                &catalog_with_counts(provider_count, models_per_provider),
                1,
                Some("\"full\"".into()),
            )
            .unwrap();
            write_cache(root.path(), &previous).unwrap();

            let partial = catalog_with_counts(1, 1);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nETag: \"partial\"\r\nConnection: close\r\n\r\n",
                    partial.len()
                );
                socket.write_all(headers.as_bytes()).unwrap();
                socket.write_all(&partial).unwrap();
            });

            let error = refresh_from_url(
                root.path(),
                Some(&previous),
                &format!("http://{address}/api.json"),
            )
            .unwrap_err();
            server.join().unwrap();
            assert!(error.contains(expected_drop), "{error}");
            assert_eq!(load_cache(root.path()).unwrap(), Some(previous));
        }
    }

    #[test]
    fn partial_catalog_cannot_drop_one_native_provider_while_totals_stay_high() {
        let mut previous = parse_catalog(&catalog_with_counts(20, 10), 1, None).unwrap();
        let provider = previous.providers.remove("provider-0").unwrap();
        previous.providers.insert("openai".into(), provider);
        let mut replacement = previous.clone();
        replacement.providers.remove("openai");
        assert!(reject_sudden_catalog_drop(&previous, &replacement)
            .unwrap_err()
            .contains("openai disappeared"));
    }
}
