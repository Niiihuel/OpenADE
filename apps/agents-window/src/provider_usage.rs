//! Account usage from provider-owned APIs, independent from model metadata.
//!
//! URLs are reviewed here; neither model listings nor account responses can
//! supply request destinations. Credentials only live in the worker and HTTP
//! headers. In particular SIWC tokens never go to Codex's private WHAM route.
//!
//! Sources: OpenRouter `/api/v1/key`, DeepSeek `/user/balance`, and OpenCode's
//! `packages/console/app/src/routes/zen/go/v1/usage.ts` (anomalyco/opencode).
//! SIWC's Accounts and sessions guide specifies linking to ChatGPT usage.

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{blocking::Client, redirect::Policy};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    native_provider::{Adapter, ProviderRegistry},
    provider_connections,
    provider_settings::ProviderConnectionView,
};

const MAX_RESPONSE_BYTES: u64 = 128 * 1024;
/// Bounds of the local Codex read: a rollout can be tens of MB, with the
/// newest limits near its end.
const ROLLOUT_TAIL_BYTES: u64 = 4 * 1024 * 1024;
const ROLLOUT_CHUNK_BYTES: u64 = 256 * 1024;
const ROLLOUT_FILES: usize = 5;
const ROLLOUT_ENTRIES: usize = 50_000;
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderUsageStatus {
    Disconnected,
    NeedsSignIn,
    Loading,
    Ready,
    External,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UsageUnit {
    Usd,
    Requests,
    Percent,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UsageWindow {
    pub label: String,
    pub used: f64,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub unit: UsageUnit,
    pub reset_at_ms: Option<u64>,
    pub reset_label: Option<String>,
}

impl UsageWindow {
    /// None means there is no published quota; it never means zero usage.
    pub(crate) fn used_percent(&self) -> Option<f64> {
        let limit = self.limit.filter(|n| n.is_finite() && *n > 0.0)?;
        let percent = self.used / limit * 100.0;
        (percent.is_finite() && percent >= 0.0).then_some(percent)
    }

    pub(crate) fn remaining_percent(&self) -> Option<f64> {
        self.used_percent()
            .map(|used| (100.0 - used).clamp(0.0, 100.0))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UsageBalance {
    pub label: String,
    pub currency: String,
    pub amount: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProviderUsageSnapshot {
    pub provider_id: String,
    pub provider_name: String,
    pub account: Option<String>,
    pub status: ProviderUsageStatus,
    /// Only the provider's response or the configured billing service supplies
    /// this label. A ChatGPT subscription tier is never inferred from models.
    pub plan_label: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub balances: Vec<UsageBalance>,
    /// Time of the latest successful measurement, retained on network errors.
    pub fetched_at_ms: Option<u64>,
    /// Read from the Codex CLI's own session files: as old as its last turn,
    /// so its age is shown as it is instead of as a failed refresh.
    pub local: bool,
    pub account_url: Option<String>,
    pub message: Option<String>,
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(crate) fn account_url(provider_id: &str) -> Option<&'static str> {
    Some(match provider_id {
        "codex" => provider_connections::CHATGPT_USAGE_URL,
        "openrouter" => "https://openrouter.ai/settings/keys",
        "opencode" => "https://opencode.ai/auth",
        "nvidia" => "https://build.nvidia.com/",
        "minimax" => "https://platform.minimax.io/console/recharge-records",
        "zai" => "https://z.ai/manage-apikey/billing",
        "deepseek" => "https://platform.deepseek.com/usage",
        "openai" => "https://platform.openai.com/usage",
        "anthropic" => "https://console.anthropic.com/settings/usage",
        "groq" => "https://console.groq.com/settings/limits",
        "mistral" => "https://console.mistral.ai/",
        "xai" => "https://console.x.ai/",
        _ => return None,
    })
}

fn plan_label(adapter: &Adapter) -> Option<String> {
    Some(
        match adapter.id.as_str() {
            "codex" => "Plan de ChatGPT",
            "openrouter" => "Créditos de API",
            "opencode" if adapter.endpoint == "https://opencode.ai/zen/go/v1" => "OpenCode Go",
            "opencode" => "OpenCode Zen",
            "nvidia" => "API de NVIDIA",
            "minimax" => "API de MiniMax",
            "zai" => "API general de Z.ai",
            "deepseek" => "Saldo de API",
            _ => return None,
        }
        .into(),
    )
}

fn usage_url(adapter: &Adapter) -> Option<&'static str> {
    match adapter.id.as_str() {
        "codex" => Some("https://chatgpt.com/backend-api/wham/usage"),
        "openrouter" => Some("https://openrouter.ai/api/v1/key"),
        "deepseek" => Some("https://api.deepseek.com/user/balance"),
        "opencode" if adapter.endpoint == "https://opencode.ai/zen/go/v1" => {
            Some("https://opencode.ai/zen/go/v1/usage")
        }
        _ => None,
    }
}

fn external_message(provider_id: &str) -> &'static str {
    match provider_id {
        "codex" => "Consulta el consumo y los límites de tu plan en ChatGPT.",
        "opencode" => "Consulta el saldo y el consumo de Zen en tu cuenta de OpenCode.",
        "nvidia" => "Consulta los límites y el consumo en tu cuenta de NVIDIA.",
        "minimax" => "Consulta el saldo y el consumo en tu cuenta de MiniMax.",
        "zai" => "Consulta el saldo y el consumo de la API en tu cuenta de Z.ai.",
        _ => "Este proveedor publica el consumo en su cuenta.",
    }
}

fn snapshot(adapter: &Adapter, root: &Path, key_available: bool) -> ProviderUsageSnapshot {
    let summary = provider_connections::summaries(root).remove(&adapter.id);
    let needs_sign_in = summary.as_ref().is_some_and(|s| s.needs_sign_in);
    let status = if needs_sign_in {
        ProviderUsageStatus::NeedsSignIn
    } else if !key_available {
        ProviderUsageStatus::Disconnected
    } else if usage_url(adapter).is_some() {
        ProviderUsageStatus::Loading
    } else {
        ProviderUsageStatus::External
    };
    let message = match status {
        ProviderUsageStatus::NeedsSignIn => {
            Some("Vuelve a conectar esta cuenta para consultar su uso.".into())
        }
        ProviderUsageStatus::Disconnected => {
            Some("Conecta tu cuenta para ver la suscripción y su uso.".into())
        }
        ProviderUsageStatus::External => Some(external_message(&adapter.id).into()),
        _ => None,
    };
    ProviderUsageSnapshot {
        provider_id: adapter.id.clone(),
        provider_name: adapter.name.clone(),
        account: summary.and_then(|s| s.account),
        status,
        plan_label: plan_label(adapter),
        windows: Vec::new(),
        balances: Vec::new(),
        fetched_at_ms: None,
        local: false,
        account_url: account_url(&adapter.id).map(str::to_owned),
        message,
    }
}

/// Local reads only; never perform HTTP on the UI thread. The visible roster
/// follows the same supported connections as Settings, rather than models.
pub(crate) fn initial_snapshots(
    root: &Path,
    connections: &BTreeMap<String, ProviderConnectionView>,
) -> BTreeMap<String, ProviderUsageSnapshot> {
    let Ok(registry) = ProviderRegistry::load(root) else {
        return BTreeMap::new();
    };
    connections
        .values()
        .filter(|connection| provider_connections::supported(&connection.id))
        .filter_map(|connection| {
            let adapter = registry.adapter(&connection.id)?;
            Some((
                connection.id.clone(),
                snapshot(
                    adapter,
                    root,
                    provider_connections::read_key(root, &adapter.key_env).is_some(),
                ),
            ))
        })
        .collect()
}

/// Runtime-only fingerprint; never put this or a credential in ViewState,
/// diagnostics, saved preferences, or catalog metadata.
pub(crate) fn credential_revision(root: &Path, provider_id: &str) -> Option<[u8; 32]> {
    let adapter = ProviderRegistry::load(root)
        .ok()?
        .adapter(provider_id)?
        .clone();
    let key = provider_connections::read_key(root, &adapter.key_env)?;
    Some(revision(&adapter, &key))
}

fn revision(adapter: &Adapter, key: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    for value in [&adapter.id, &adapter.endpoint, &adapter.key_env, key] {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value.as_bytes());
    }
    digest.finalize().into()
}

/// The caller must first verify the credential revision is unchanged. Failed
/// refreshes retain the last actual measurement, with its original timestamp.
/// A rejected credential or disconnected account always clears old usage.
pub(crate) fn retain_last_success(
    previous: &ProviderUsageSnapshot,
    mut incoming: ProviderUsageSnapshot,
) -> ProviderUsageSnapshot {
    if incoming.provider_id == previous.provider_id
        && incoming.status == ProviderUsageStatus::Error
        && previous.fetched_at_ms.is_some()
    {
        incoming.plan_label = previous.plan_label.clone();
        incoming.windows = previous.windows.clone();
        incoming.balances = previous.balances.clone();
        incoming.fetched_at_ms = previous.fetched_at_ms;
        incoming.message = Some(format!(
            "{} Se conserva el último dato recibido.",
            incoming
                .message
                .as_deref()
                .unwrap_or("No se pudo actualizar el uso.")
        ));
    }
    incoming
}

pub(crate) struct UsageWorker {
    pub receiver: Receiver<Result<BTreeMap<String, ProviderUsageSnapshot>, String>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for UsageWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl UsageWorker {
    #[cfg(test)]
    pub(crate) fn from_receiver(
        receiver: Receiver<Result<BTreeMap<String, ProviderUsageSnapshot>, String>>,
    ) -> Self {
        Self {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn spawn(root: PathBuf, provider_ids: Vec<String>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = cancel.clone();
        thread::spawn(move || {
            let result = collect_usage(&root, &provider_ids, &task_cancel);
            if !task_cancel.load(Ordering::Relaxed) {
                let _ = sender.send(result);
            }
        });
        Self { receiver, cancel }
    }
}

fn collect_usage(
    root: &Path,
    provider_ids: &[String],
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, ProviderUsageSnapshot>, String> {
    let registry = ProviderRegistry::load(root)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(5))
        .redirect(Policy::none())
        .user_agent("OpenADE/0.1")
        .build()
        .map_err(|_| "No se pudo preparar la consulta de uso".to_owned())?;
    let mut result = BTreeMap::new();
    for id in provider_ids {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(adapter) = registry.adapter(id) else {
            continue;
        };
        // A ChatGPT sign-in is renewed here like before any turn; its stored
        // token alone reads as missing once it has expired.
        let key = if adapter.id == "codex" {
            openide_connections::codex_token_source(root.to_path_buf())
                .ok()
                .and_then(|token| token().ok())
        } else {
            provider_connections::read_key(root, &adapter.key_env)
        };
        let original_revision = credential_revision(root, id);
        let account = (adapter.id == "codex")
            .then(|| chatgpt_account_id(root))
            .flatten();
        let mut state = snapshot(adapter, root, key.is_some());
        // Only a sign-in that names its ChatGPT workspace can read the plan.
        // Any other one shows what the Codex CLI last recorded on this machine,
        // or keeps pointing at the account page; neither sends a request.
        if adapter.id == "codex"
            && account.is_none()
            && state.status == ProviderUsageStatus::Loading
            // Tests never read the real Codex home.
            && !(!cfg!(test)
                && crate::composer_selection::codex_home()
                    .is_some_and(|home| apply_local_codex(&mut state, &home, now_ms())))
        {
            state.status = ProviderUsageStatus::External;
            state.message = Some(external_message(&adapter.id).into());
        }
        if state.status == ProviderUsageStatus::Loading {
            if let (Some(url), Some(key)) = (usage_url(adapter), key.as_deref()) {
                match query_usage(&client, url, key, account.as_deref(), adapter, &mut state) {
                    Ok(()) => {
                        state.status = ProviderUsageStatus::Ready;
                        state.fetched_at_ms = Some(now_ms());
                    }
                    Err(error) => {
                        state.status = error.status;
                        state.message = Some(error.message.into());
                    }
                }
            }
        }
        // A credential may have been replaced/disconnected in another process
        // while HTTP was in flight; such a measurement belongs to the old key.
        if !cancel.load(Ordering::Relaxed) && credential_revision(root, id) == original_revision {
            result.insert(id.clone(), state);
        }
    }
    Ok(result)
}

struct UsageError {
    status: ProviderUsageStatus,
    message: &'static str,
}

impl UsageError {
    fn unavailable(message: &'static str) -> Self {
        Self {
            status: ProviderUsageStatus::Error,
            message,
        }
    }
}

/// The ChatGPT workspace the sign-in belongs to, as stated by its identity token.
fn chatgpt_account_id(root: &Path) -> Option<String> {
    use base64::Engine;
    let token = openide_connections::load(root, "codex").ok()??.id_token?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1)?)
        .ok()?;
    let claims: Value = serde_json::from_slice(&payload).ok()?;
    claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 128 && id.is_ascii())
        .map(str::to_owned)
}

fn query_usage(
    client: &Client,
    url: &str,
    key: &str,
    account: Option<&str>,
    adapter: &Adapter,
    state: &mut ProviderUsageSnapshot,
) -> Result<(), UsageError> {
    let mut request = client
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json");
    if let Some(account) = account {
        request = request.header("chatgpt-account-id", account);
    }
    let mut response = request
        .send()
        .map_err(|_| {
            UsageError::unavailable("No se pudo contactar al proveedor. Inténtalo de nuevo.")
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            // The sign-in still works for turns; it just cannot read the plan.
            401 | 403 if adapter.id == "codex" => UsageError {
                status: ProviderUsageStatus::External,
                message: "Consulta el consumo y los límites de tu plan en ChatGPT.",
            },
            401 => UsageError {
                status: ProviderUsageStatus::NeedsSignIn,
                message: "El proveedor rechazó esta conexión. Vuelve a conectar la cuenta.",
            },
            403 if adapter.id == "opencode" => UsageError {
                status: ProviderUsageStatus::External,
                message: "Esta clave no permite consultar una suscripción Go. Revisa tu cuenta de OpenCode.",
            },
            403 => UsageError {
                status: ProviderUsageStatus::External,
                message: "Esta conexión no tiene permiso para consultar el uso.",
            },
            429 => UsageError::unavailable(
                "El proveedor limitó las consultas de uso. Inténtalo más tarde.",
            ),
            _ => UsageError::unavailable(
                "El proveedor no pudo actualizar el uso. Inténtalo de nuevo.",
            ),
        });
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UsageError::unavailable("No se pudo leer el uso del proveedor."))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(UsageError::unavailable(
            "La respuesta de uso excede el tamaño permitido.",
        ));
    }
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| UsageError::unavailable("El proveedor devolvió datos de uso inválidos."))?;
    let mut updated = state.clone();
    let parsed = match adapter.id.as_str() {
        "codex" => parse_codex(&body, &mut updated),
        "openrouter" => parse_openrouter(&body, &mut updated),
        "opencode" => parse_opencode_go(&body, &mut updated),
        "deepseek" => parse_deepseek(&body, &mut updated),
        _ => Err("Este proveedor no publica una API de uso compatible."),
    };
    parsed.map_err(UsageError::unavailable)?;
    *state = updated;
    Ok(())
}

fn number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    let result = value.as_f64().or_else(|| value.as_str()?.parse().ok())?;
    result.is_finite().then_some(result)
}

fn nonnegative(value: Option<&Value>) -> Option<f64> {
    number(value).filter(|value| *value >= 0.0)
}

fn usage_window(label: &str, used: f64, unit: UsageUnit) -> UsageWindow {
    UsageWindow {
        label: label.into(),
        used,
        limit: None,
        remaining: None,
        unit,
        reset_at_ms: None,
        reset_label: None,
    }
}

fn parse_openrouter(body: &Value, state: &mut ProviderUsageSnapshot) -> Result<(), &'static str> {
    let data = body
        .get("data")
        .and_then(Value::as_object)
        .ok_or("OpenRouter no devolvió información válida de esta clave.")?;
    let total_usage =
        nonnegative(data.get("usage")).ok_or("OpenRouter no devolvió un consumo válido.")?;
    let plan_label = Some(
        if data.get("is_free_tier").and_then(Value::as_bool) == Some(true) {
            "API · gratuita"
        } else {
            "Créditos de API"
        }
        .into(),
    );
    let mut windows = Vec::new();
    if let Some(limit) = nonnegative(data.get("limit")) {
        let reset = data.get("limit_reset").and_then(Value::as_str);
        let include_byok = data.get("include_byok_in_limit").and_then(Value::as_bool) == Some(true);
        let period = match reset {
            Some("daily") => "usage_daily",
            Some("weekly") => "usage_weekly",
            Some("monthly") => "usage_monthly",
            None => "usage",
            _ => "",
        };
        let period_usage = nonnegative(data.get(period)).map(|used| {
            used + if include_byok {
                nonnegative(data.get(&format!("byok_{period}"))).unwrap_or(0.0)
            } else {
                0.0
            }
        });
        let remaining = number(data.get("limit_remaining"));
        if remaining.is_some_and(|remaining| remaining > limit) {
            return Err("OpenRouter devolvió un límite inconsistente.");
        }
        // limit_remaining is the provider's authoritative remaining key budget;
        // lifetime usage is not a monthly numerator when a reset is configured.
        if let Some(used) = remaining
            .map(|remaining| limit - remaining)
            .or(period_usage)
        {
            if !used.is_finite() || used < 0.0 {
                return Err("OpenRouter devolvió un límite inconsistente.");
            }
            let mut window = usage_window("Límite de la clave", used, UsageUnit::Usd);
            window.limit = Some(limit);
            window.remaining = remaining.or_else(|| Some((limit - used).max(0.0)));
            window.reset_label = match reset {
                Some("daily") => Some("Reinicio diario".into()),
                Some("weekly") => Some("Reinicio semanal".into()),
                Some("monthly") => Some("Reinicio mensual".into()),
                _ => None,
            };
            windows.push(window);
        }
    }
    if windows.is_empty() {
        windows.push(usage_window(
            "Consumo acumulado",
            total_usage,
            UsageUnit::Usd,
        ));
    }
    if let Some(free) = data
        .get("free_model_daily_requests")
        .and_then(Value::as_object)
    {
        if let (Some(used), Some(limit)) = (
            nonnegative(free.get("used")),
            nonnegative(free.get("limit")),
        ) {
            let remaining = nonnegative(free.get("remaining"));
            if remaining.is_some_and(|remaining| remaining > limit) {
                return Err("OpenRouter devolvió un límite de solicitudes inconsistente.");
            }
            let mut window = usage_window("Modelos gratuitos · hoy", used, UsageUnit::Requests);
            window.limit = Some(limit);
            window.remaining = remaining;
            window.reset_label = Some("Reinicio diario".into());
            windows.push(window);
        }
    }
    // Key metadata is not the account's credit balance. /credits requires a
    // management key, so no account-wide balance is guessed or requested here.
    state.plan_label = plan_label;
    state.windows = windows;
    state.balances.clear();
    state.message =
        Some("Consumo de la clave conectada; el saldo total se consulta en OpenRouter.".into());
    Ok(())
}

/// A ChatGPT plan reports how much of each rolling window is used.
fn parse_codex(body: &Value, state: &mut ProviderUsageSnapshot) -> Result<(), &'static str> {
    let limits = body
        .get("rate_limit")
        .and_then(Value::as_object)
        .ok_or("ChatGPT no devolvió los límites del plan.")?;
    let mut windows = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        let Some(row) = limits.get(key).filter(|row| !row.is_null()) else {
            continue;
        };
        let used = nonnegative(row.get("used_percent"))
            .ok_or("ChatGPT devolvió un porcentaje de uso inválido.")?
            .min(100.0);
        let seconds = nonnegative(row.get("limit_window_seconds")).unwrap_or(0.0) as u64;
        let label = match seconds {
            18_000 => "Últimas 5 horas".to_owned(),
            604_800 => "Semana".to_owned(),
            0 => "Límite del plan".to_owned(),
            seconds if seconds % 86_400 == 0 => format!("Últimos {} días", seconds / 86_400),
            seconds if seconds >= 3_600 => format!("Últimas {} horas", seconds / 3_600),
            seconds => format!("Últimos {} minutos", (seconds / 60).max(1)),
        };
        let mut window = usage_window(&label, used, UsageUnit::Percent);
        window.limit = Some(100.0);
        window.remaining = Some(100.0 - used);
        window.reset_at_ms = nonnegative(row.get("reset_at"))
            .map(|seconds| (seconds * 1000.0) as u64)
            .filter(|at| *at > 0);
        windows.push(window);
    }
    if windows.is_empty() {
        return Err("ChatGPT no devolvió ventanas de uso válidas.");
    }
    state.windows = windows;
    state.balances.clear();
    if let Some(plan) = body.get("plan_type").and_then(Value::as_str) {
        state.plan_label = Some(chatgpt_plan(plan));
    }
    let reached = limits.get("limit_reached").and_then(Value::as_bool) == Some(true)
        || limits.get("allowed").and_then(Value::as_bool) == Some(false);
    state.message =
        reached.then(|| "Se alcanzó un límite del plan. Revisa su fecha de reinicio.".into());
    Ok(())
}

fn chatgpt_plan(plan: &str) -> String {
    let mut name = plan.replace(['_', '-'], " ");
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    format!("ChatGPT {name}")
}

/// Newest first by modification time: a resumed session appends to its old file.
fn newest_rollouts(home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![(home.join("sessions"), 0)];
    let mut visited = 0;
    'walk: while let Some((directory, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > ROLLOUT_ENTRIES {
                break 'walk;
            }
            // Not followed through links, so the walk cannot loop.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_dir() && depth < 3 {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file() && name.starts_with("rollout-") && name.ends_with(".jsonl") {
                if let Ok(modified) = entry.metadata().and_then(|file| file.modified()) {
                    found.push((modified, entry.path()));
                }
            }
        }
    }
    found.sort_by(|a, b| b.cmp(a));
    found.truncate(ROLLOUT_FILES);
    found.into_iter().map(|(_, path)| path).collect()
}

/// The plan's limits and their time in ms, when the line is such an event.
fn rate_limits_line(line: &[u8]) -> Option<(Value, u64)> {
    let line = std::str::from_utf8(line).ok()?;
    if !line.contains("\"token_count\"") || !line.contains("\"rate_limits\"") {
        return None;
    }
    let mut event: Value = serde_json::from_str(line).ok()?;
    if event["type"] != "event_msg" || event["payload"]["type"] != "token_count" {
        return None;
    }
    let at = chrono::DateTime::parse_from_rfc3339(event["timestamp"].as_str()?)
        .ok()
        .and_then(|at| u64::try_from(at.timestamp_millis()).ok())?;
    let limits = event["payload"]["rate_limits"].take();
    // Other metered limits use the same event; only "codex" is the plan itself.
    let own = limits["limit_id"].is_null() || limits["limit_id"] == "codex";
    (limits.is_object() && own).then_some((limits, at))
}

/// Reads backwards in chunks and never past the last `ROLLOUT_TAIL_BYTES`.
fn last_rate_limits(path: &Path) -> Option<(Value, u64)> {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut end = file.metadata().ok()?.len();
    let floor = end.saturating_sub(ROLLOUT_TAIL_BYTES);
    let mut carry = Vec::new();
    while end > floor {
        let start = end.saturating_sub(ROLLOUT_CHUNK_BYTES).max(floor);
        let mut buffer = vec![0; (end - start) as usize];
        file.seek(SeekFrom::Start(start)).ok()?;
        file.read_exact(&mut buffer).ok()?;
        buffer.append(&mut carry);
        // A chunk begins inside a line unless it begins the file.
        let whole = if start == 0 {
            0
        } else {
            buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1)
        };
        if let Some(found) = buffer[whole..]
            .rsplit(|byte| *byte == b'\n')
            .find_map(rate_limits_line)
        {
            return Some(found);
        }
        buffer.truncate(whole);
        carry = buffer;
        end = start;
    }
    None
}

/// What the Codex CLI last recorded in its own session files under `home`.
/// Local files only, every field untrusted; false leaves `state` untouched.
fn apply_local_codex(state: &mut ProviderUsageSnapshot, home: &Path, now: u64) -> bool {
    let Some((limits, at)) = newest_rollouts(home)
        .iter()
        .find_map(|path| last_rate_limits(path))
    else {
        return false;
    };
    let mut windows = Vec::new();
    // The plan decides which window is primary; only its length names it.
    for key in ["primary", "secondary"] {
        let row = &limits[key];
        let Some(used) = number(row.get("used_percent")) else {
            continue;
        };
        let used = used.clamp(0.0, 100.0);
        let reset_at_ms = nonnegative(row.get("resets_at"))
            .map(|seconds| (seconds * 1000.0) as u64)
            .filter(|reset| *reset > 0 && *reset < now + 366 * 86_400_000);
        // A window that has reset since the event no longer holds this percent.
        if reset_at_ms.is_some_and(|reset| reset <= now) {
            continue;
        }
        let label = match nonnegative(row.get("window_minutes")).map_or(0, |minutes| minutes as u64)
        {
            300 => "5 horas".to_owned(),
            1_440 => "Diario".to_owned(),
            10_080 => "Semanal".to_owned(),
            0 => "Límite del plan".to_owned(),
            minutes if minutes % 1_440 == 0 => format!("{} días", minutes / 1_440),
            minutes if minutes % 60 == 0 => format!("{} horas", minutes / 60),
            minutes => format!("{minutes} min"),
        };
        let mut window = usage_window(&label, used, UsageUnit::Percent);
        window.limit = Some(100.0);
        window.remaining = Some(100.0 - used);
        window.reset_at_ms = reset_at_ms;
        windows.push(window);
    }
    if windows.is_empty() {
        return false;
    }
    state.status = ProviderUsageStatus::Ready;
    state.windows = windows;
    state.balances.clear();
    if let Some(plan) = limits["plan_type"].as_str().filter(|plan| !plan.is_empty()) {
        state.plan_label = Some(chatgpt_plan(plan));
    }
    state.fetched_at_ms = Some(at.min(now));
    state.local = true;
    state.message = Some("Límites de tu última sesión local de Codex.".into());
    true
}

fn parse_opencode_go(body: &Value, state: &mut ProviderUsageSnapshot) -> Result<(), &'static str> {
    let usage = body
        .get("usage")
        .and_then(Value::as_object)
        .ok_or("OpenCode Go no devolvió ventanas de uso válidas.")?;
    let mut limited = false;
    let mut windows = Vec::new();
    for (key, label) in [
        ("rolling", "Últimas 5 horas"),
        ("weekly", "Semana"),
        ("monthly", "Mes"),
    ] {
        let Some(row) = usage.get(key) else {
            continue;
        };
        let used = nonnegative(row.get("percent"))
            .ok_or("OpenCode Go devolvió un porcentaje de uso inválido.")?;
        let mut window = usage_window(label, used, UsageUnit::Percent);
        window.limit = Some(100.0);
        window.remaining = Some((100.0 - used).max(0.0));
        window.reset_at_ms = row
            .get("resetsAt")
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .and_then(|value| u64::try_from(value.timestamp_millis()).ok());
        limited |= row.get("status").and_then(Value::as_str) == Some("rate-limited");
        windows.push(window);
    }
    if windows.is_empty() {
        return Err("OpenCode Go no devolvió ventanas de uso válidas.");
    }
    state.windows = windows;
    state.balances.clear();
    state.plan_label = Some("OpenCode Go".into());
    state.message =
        limited.then(|| "Se alcanzó un límite del plan. Revisa su fecha de reinicio.".into());
    Ok(())
}

fn parse_deepseek(body: &Value, state: &mut ProviderUsageSnapshot) -> Result<(), &'static str> {
    let rows = body
        .get("balance_infos")
        .and_then(Value::as_array)
        .ok_or("DeepSeek no devolvió un saldo válido.")?;
    let mut balances = Vec::new();
    for row in rows {
        let currency = row
            .get("currency")
            .and_then(Value::as_str)
            .filter(|currency| matches!(*currency, "USD" | "CNY"))
            .ok_or("DeepSeek devolvió una moneda de saldo no compatible.")?;
        let amount =
            number(row.get("total_balance")).ok_or("DeepSeek no devolvió un saldo válido.")?;
        balances.push(UsageBalance {
            label: "Saldo disponible".into(),
            currency: currency.into(),
            amount,
        });
    }
    if balances.is_empty() {
        return Err("DeepSeek no devolvió un saldo válido.");
    }
    state.windows.clear();
    state.balances = balances;
    state.message = (body.get("is_available").and_then(Value::as_bool) == Some(false))
        .then(|| "El proveedor indica que el saldo no alcanza para nuevas solicitudes.".into());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    fn state(id: &str) -> ProviderUsageSnapshot {
        let root = tempfile::tempdir().unwrap();
        snapshot(
            ProviderRegistry::builtin().adapter(id).unwrap(),
            root.path(),
            true,
        )
    }

    fn save_fixture(root: &Path, id: &str, disabled: bool) {
        let directory = root.join("provider-credentials");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{id}.json"));
        std::fs::write(
            &path,
            json!({
                "version":1,"provider_id":id,"access_token":"placeholder-key",
                "disabled":disabled,"service":"zen"
            })
            .to_string(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn openrouter_uses_remaining_budget_instead_of_lifetime_usage_and_ignores_key_label() {
        let mut state = state("openrouter");
        parse_openrouter(
            &json!({"data": {
                "label":"sk-or-v1-secret-part", "limit":100.0, "limit_remaining":74.5,
                "limit_reset":"monthly", "usage":850.0, "usage_monthly":25.5,
                "is_free_tier":false,
                "free_model_daily_requests":{"limit":50,"used":12,"remaining":38}
            }}),
            &mut state,
        )
        .unwrap();
        assert_eq!(state.windows[0].used, 25.5);
        assert_eq!(state.windows[0].used_percent(), Some(25.5));
        assert_eq!(state.windows[0].remaining_percent(), Some(74.5));
        assert_eq!(state.windows[0].reset_at_ms, None);
        assert_eq!(state.windows[1].used_percent(), Some(24.0));
        assert!(state.balances.is_empty());
        assert!(!format!("{state:?}").contains("secret-part"));
    }

    #[test]
    fn unlimited_keys_do_not_become_an_invented_percentage_or_account_balance() {
        let mut state = state("openrouter");
        parse_openrouter(
            &json!({"data":{"usage":4.2,"limit":null,"limit_remaining":null}}),
            &mut state,
        )
        .unwrap();
        assert_eq!(state.windows[0].used, 4.2);
        assert_eq!(state.windows[0].used_percent(), None);
        assert_eq!(state.windows[0].remaining_percent(), None);
        assert!(state.balances.is_empty());
    }

    #[test]
    fn openrouter_period_limit_includes_byok_only_when_provider_says_so() {
        let mut state = state("openrouter");
        parse_openrouter(
            &json!({"data":{"usage":450,"usage_weekly":8,
            "limit":20,"limit_reset":"weekly","include_byok_in_limit":true,
            "byok_usage_weekly":2}}),
            &mut state,
        )
        .unwrap();
        assert_eq!(state.windows[0].used_percent(), Some(50.0));
        assert_eq!(state.windows[0].remaining, Some(10.0));
    }

    #[test]
    fn chatgpt_plan_reports_each_window_with_what_is_left_and_when_it_resets() {
        let mut usage = state("codex");
        parse_codex(
            &json!({
                "plan_type": "plus",
                "rate_limit": {
                    "allowed": true,
                    "limit_reached": false,
                    "primary_window": {
                        "used_percent": 42,
                        "limit_window_seconds": 18000,
                        "reset_after_seconds": 600,
                        "reset_at": 1735693200
                    },
                    "secondary_window": {
                        "used_percent": 5,
                        "limit_window_seconds": 604800,
                        "reset_at": 1736000000
                    }
                },
                "additional_rate_limits": [{ "limit_name": "codex_other" }]
            }),
            &mut usage,
        )
        .unwrap();
        assert_eq!(usage.plan_label.as_deref(), Some("ChatGPT Plus"));
        assert!(usage.message.is_none() && usage.balances.is_empty());
        let rows: Vec<_> = usage
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.used, w.remaining, w.reset_at_ms, w.unit))
            .collect();
        assert_eq!(
            rows,
            [
                ("Últimas 5 horas", 42.0, Some(58.0), Some(1_735_693_200_000), UsageUnit::Percent),
                ("Semana", 5.0, Some(95.0), Some(1_736_000_000_000), UsageUnit::Percent),
            ]
        );
        assert_eq!(usage.windows[0].remaining_percent(), Some(58.0));

        // One window only, an unusual length, and a reached limit.
        let mut reached = state("codex");
        parse_codex(
            &json!({
                "plan_type": "pro_plus",
                "rate_limit": {
                    "limit_reached": true,
                    "primary_window": { "used_percent": 130, "limit_window_seconds": 1800 },
                    "secondary_window": null
                }
            }),
            &mut reached,
        )
        .unwrap();
        assert_eq!(reached.plan_label.as_deref(), Some("ChatGPT Pro plus"));
        assert_eq!(reached.windows.len(), 1);
        assert_eq!(reached.windows[0].label, "Últimos 30 minutos");
        assert_eq!((reached.windows[0].used, reached.windows[0].remaining), (100.0, Some(0.0)));
        assert!(reached.windows[0].reset_at_ms.is_none());
        assert!(reached.message.as_deref().unwrap().contains("límite"));

        // Nothing usable leaves the previous state untouched.
        let mut untouched = state("codex");
        let before = untouched.clone();
        for body in [json!({}), json!({ "rate_limit": {} }), json!({ "rate_limit": { "primary_window": { "used_percent": "x" } } })] {
            assert!(parse_codex(&body, &mut untouched).is_err());
        }
        assert_eq!(untouched.windows, before.windows);
        assert_eq!(usage_url(ProviderRegistry::builtin().adapter("codex").unwrap()), Some("https://chatgpt.com/backend-api/wham/usage"));
    }

    const NOW: u64 = 1_800_000_000_000;

    fn rollout(home: &Path, day: &str, name: &str, lines: &[String], age_seconds: u64) -> PathBuf {
        let directory = home.join("sessions").join(day);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("rollout-{name}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(age_seconds))
            .unwrap();
        path
    }

    fn token_count(timestamp: &str, limits: Value) -> String {
        json!({"timestamp": timestamp, "type": "event_msg",
            "payload": {"type": "token_count", "info": null, "rate_limits": limits}})
        .to_string()
    }

    fn local(home: &Path) -> Option<ProviderUsageSnapshot> {
        let mut usage = state("codex");
        apply_local_codex(&mut usage, home, NOW).then_some(usage)
    }

    fn week(used: f64) -> Value {
        json!({"used_percent": used, "window_minutes": 10080, "resets_at": NOW / 1000 + 259_200})
    }

    #[test]
    fn local_codex_limits_name_each_window_by_its_length_and_keep_the_event_time() {
        let home = tempfile::tempdir().unwrap();
        rollout(
            home.path(),
            "2027/01/15",
            "weekly",
            &[
                token_count("2027-01-15T07:00:00.000Z", json!({"primary": week(12.0)})),
                "{not json \"token_count\" \"rate_limits\"".into(),
                json!({"timestamp":"2027-01-15T07:30:00Z","type":"response_item",
                    "payload":{"text":"\"token_count\" \"rate_limits\""}})
                .to_string(),
                token_count(
                    "2027-01-15T08:00:00.000Z",
                    json!({"limit_id": "codex", "primary": week(58.0), "secondary": null,
                        "credits": null, "plan_type": "pro"}),
                ),
                // Later lines without limits, or for another meter, are passed over.
                token_count("2027-01-15T08:01:00.000Z", Value::Null),
                token_count(
                    "2027-01-15T08:02:00.000Z",
                    json!({"limit_id": "codex_other", "primary": week(99.0)}),
                ),
                "truncated {\"type\":\"event_msg\",\"token_count\" \"rate_limits\"".into(),
            ],
            0,
        );
        let usage = local(home.path()).unwrap();
        assert_eq!(usage.status, ProviderUsageStatus::Ready);
        assert!(usage.local && usage.balances.is_empty());
        assert_eq!(usage.plan_label.as_deref(), Some("ChatGPT Pro"));
        assert_eq!(usage.fetched_at_ms, Some(NOW));
        assert_eq!(usage.windows.len(), 1);
        let window = &usage.windows[0];
        assert_eq!((window.label.as_str(), window.used, window.remaining), ("Semanal", 58.0, Some(42.0)));
        assert_eq!((window.unit, window.limit), (UsageUnit::Percent, Some(100.0)));
        assert_eq!(window.reset_at_ms, Some(NOW + 259_200_000));

        // Both windows, in the file's order, whichever one the plan calls primary.
        let both = tempfile::tempdir().unwrap();
        rollout(
            both.path(),
            "2027/01/15",
            "both",
            &[token_count(
                "2027-01-15T07:59:00.000Z",
                json!({"primary": {"used_percent": 30, "window_minutes": 300, "resets_at": NOW / 1000 + 600},
                    "secondary": week(85.0), "plan_type": "plus"}),
            )],
            0,
        );
        let usage = local(both.path()).unwrap();
        let rows: Vec<_> = usage.windows.iter().map(|w| (w.label.as_str(), w.used)).collect();
        assert_eq!(rows, [("5 horas", 30.0), ("Semanal", 85.0)]);
        assert_eq!(usage.fetched_at_ms, Some(NOW - 60_000));
        assert_eq!(usage.plan_label.as_deref(), Some("ChatGPT Plus"));
    }

    #[test]
    fn local_codex_limits_distrust_every_field_and_missing_data_stays_external() {
        let home = tempfile::tempdir().unwrap();
        // No directory, an empty one, and a file instead of the folder.
        assert!(local(&home.path().join("absent")).is_none());
        assert!(local(home.path()).is_none());
        std::fs::write(home.path().join("sessions"), "x").unwrap();
        assert!(local(home.path()).is_none());
        std::fs::remove_file(home.path().join("sessions")).unwrap();

        // Null windows, an unusable percent and a missing timestamp yield nothing.
        let before = state("codex");
        for (index, line) in [
            token_count("2027-01-15T08:00:00Z", json!({"primary": null, "secondary": null})),
            token_count("2027-01-15T08:00:00Z", json!({"primary": {"used_percent": "NaN"}})),
            token_count("2027-01-15T08:00:00Z", json!({"primary": "x", "secondary": [1]})),
            token_count("sometime", json!({"primary": week(40.0)})),
            // The window has reset since: its old percent is no longer true.
            token_count(
                "2027-01-01T08:00:00Z",
                json!({"primary": {"used_percent": 90, "window_minutes": 10080, "resets_at": NOW / 1000 - 1}}),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let case = tempfile::tempdir().unwrap();
            rollout(case.path(), "2027/01/15", "bad", &[line], 0);
            let mut usage = before.clone();
            assert!(!apply_local_codex(&mut usage, case.path(), NOW), "case {index}");
            assert_eq!(usage, before);
        }

        // Out-of-range values are clamped; absurd resets and lengths are dropped.
        rollout(
            home.path(),
            "2027/01/15",
            "odd",
            &[token_count(
                "2030-01-01T00:00:00Z",
                json!({"primary": {"used_percent": 250.5, "window_minutes": 90, "resets_at": 9.9e15},
                    "secondary": {"used_percent": -4, "window_minutes": -3, "resets_at": -1},
                    "plan_type": 7}),
            )],
            0,
        );
        let usage = local(home.path()).unwrap();
        let rows: Vec<_> = usage
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.used, w.remaining, w.reset_at_ms))
            .collect();
        assert_eq!(
            rows,
            [("90 min", 100.0, Some(0.0), None), ("Límite del plan", 0.0, Some(100.0), None)]
        );
        // A clock ahead of ours never makes the data newer than now.
        assert_eq!(usage.fetched_at_ms, Some(NOW));
        assert_eq!(usage.plan_label.as_deref(), Some("Plan de ChatGPT"));
    }

    #[test]
    fn local_codex_limits_come_from_the_newest_file_and_only_from_its_tail() {
        let home = tempfile::tempdir().unwrap();
        let line = |used| token_count("2027-01-15T08:00:00Z", json!({"primary": week(used)}));
        // The newest file is the one written last, not the one in the latest folder.
        rollout(home.path(), "2027/01/15", "old", &[line(10.0)], 3_600);
        rollout(home.path(), "2027/01/02", "resumed", &[line(20.0)], 60);
        std::fs::write(home.path().join("sessions/2027/01/15/notes.jsonl"), line(99.0)).unwrap();
        assert_eq!(local(home.path()).unwrap().windows[0].used, 20.0);
        // A newer file without limits falls back to the previous one.
        rollout(home.path(), "2027/01/16", "empty", &["{\"type\":\"session_meta\"}".into()], 0);
        assert_eq!(local(home.path()).unwrap().windows[0].used, 20.0);
        assert_eq!(newest_rollouts(home.path()).len(), 3);

        // A large file: the event is found across chunk boundaries, after a
        // line longer than a chunk, without reading the head of the file.
        let filler = format!("{{\"type\":\"response_item\",\"text\":\"{}\"}}", "x".repeat(1_000));
        let mut lines = vec![line(1.0)];
        lines.extend(std::iter::repeat_n(filler.clone(), 6_000));
        lines.push(line(64.0));
        lines.push(format!("{{\"text\":\"{}\"}}", "y".repeat(300_000)));
        lines.extend(std::iter::repeat_n(filler.clone(), 300));
        let large = rollout(home.path(), "2027/01/17", "large", &lines, 0);
        assert!(std::fs::metadata(&large).unwrap().len() > ROLLOUT_TAIL_BYTES);
        assert_eq!(local(home.path()).unwrap().windows[0].used, 64.0);
        // Beyond the scanned tail the event is not looked for; older files are.
        let mut lines = vec![line(2.0)];
        lines.extend(std::iter::repeat_n(filler, 4_500));
        rollout(home.path(), "2027/01/18", "deep", &lines, 0);
        assert_eq!(local(home.path()).unwrap().windows[0].used, 64.0);
    }

    #[test]
    #[ignore = "reads the signed-in ChatGPT plan once: OPENIDE_LIVE_DATA_ROOT=<data folder>"]
    fn live_chatgpt_plan_usage_is_readable_with_the_saved_sign_in() {
        let root = std::path::PathBuf::from(std::env::var("OPENIDE_LIVE_DATA_ROOT").unwrap());
        let cancel = AtomicBool::new(false);
        let result = collect_usage(&root, &["codex".to_owned()], &cancel).unwrap();
        let usage = result.get("codex").expect("a codex measurement");
        println!(
            "LIVE usage: status {:?}, plan {:?}, message {:?}, account header {}",
            usage.status,
            usage.plan_label,
            usage.message,
            chatgpt_account_id(&root).is_some()
        );
        for window in &usage.windows {
            println!(
                "LIVE usage: {} used {:.0}% remaining {:?} resets {:?}",
                window.label, window.used, window.remaining, window.reset_at_ms
            );
        }
        match usage.status {
            ProviderUsageStatus::Ready => assert!(!usage.windows.is_empty()),
            // A sign-in without a ChatGPT workspace must not look broken.
            status => assert_eq!(status, ProviderUsageStatus::External),
        }
    }

    #[test]
    fn go_accepts_real_server_windows_without_fabricating_missing_reset_or_month() {
        let mut state = state("opencode");
        parse_opencode_go(
            &json!({"usage": {
                "rolling":{"percent":30,"resetsAt":"2026-09-23T17:00:00.000Z","status":"ok"},
                "weekly":{"percent":101,"resetsAt":"invalid","status":"rate-limited"}
            }}),
            &mut state,
        )
        .unwrap();
        assert_eq!(state.windows.len(), 2);
        assert_eq!(state.windows[0].used_percent(), Some(30.0));
        assert_eq!(state.windows[0].reset_at_ms, Some(1790182800000));
        assert_eq!(state.windows[1].reset_at_ms, None);
        assert_eq!(state.windows[1].remaining_percent(), Some(0.0));
        assert!(state.message.is_some());
    }

    #[test]
    fn malformed_or_nonfinite_measurements_are_rejected() {
        for body in [
            json!({"usage":{}}),
            json!({"usage":{"rolling":{"percent":-1}}}),
            json!({"usage":{"rolling":{"percent":"NaN"}}}),
        ] {
            assert!(parse_opencode_go(&body, &mut state("opencode")).is_err());
        }
        assert!(parse_openrouter(
            &json!({"data":{"usage":5,"limit":10,"limit_remaining":11}}),
            &mut state("openrouter")
        )
        .is_err());
        assert!(parse_deepseek(
            &json!({"balance_infos":[{"currency":"USD","total_balance":"inf"}]}),
            &mut state("deepseek")
        )
        .is_err());
    }

    #[test]
    fn parsers_publish_atomically_when_a_later_measurement_is_invalid() {
        for (id, body) in [
            (
                "opencode",
                json!({"usage": {
                    "rolling":{"percent":42}, "weekly":{"percent":"NaN"}
                }}),
            ),
            (
                "deepseek",
                json!({"balance_infos":[
                    {"currency":"USD","total_balance":"12"},
                    {"currency":"USD","total_balance":"NaN"}
                ]}),
            ),
            (
                "openrouter",
                json!({"data":{
                    "usage":5,"limit":20,"limit_remaining":15,
                    "free_model_daily_requests":{"limit":10,"used":1,"remaining":11}
                }}),
            ),
        ] {
            let mut snapshot = state(id);
            let original = snapshot.clone();
            let result = match id {
                "opencode" => parse_opencode_go(&body, &mut snapshot),
                "deepseek" => parse_deepseek(&body, &mut snapshot),
                _ => parse_openrouter(&body, &mut snapshot),
            };
            assert!(result.is_err());
            assert_eq!(snapshot, original);
        }
    }

    #[test]
    fn deepseek_keeps_currencies_distinct_and_never_creates_a_quota() {
        let mut state = state("deepseek");
        parse_deepseek(
            &json!({"is_available":true,"balance_infos":[
                {"currency":"USD","total_balance":"11.25"},
                {"currency":"CNY","total_balance":"80.10"}
            ]}),
            &mut state,
        )
        .unwrap();
        assert_eq!(state.balances[0].amount, 11.25);
        assert_eq!(state.balances[1].currency, "CNY");
        assert!(state.windows.is_empty());
    }

    #[test]
    fn stale_errors_keep_original_measurement_but_rejected_credentials_clear_it() {
        let mut previous = state("openrouter");
        parse_openrouter(&json!({"data":{"usage":5}}), &mut previous).unwrap();
        previous.status = ProviderUsageStatus::Ready;
        previous.fetched_at_ms = Some(42);
        let mut failed = state("openrouter");
        failed.status = ProviderUsageStatus::Error;
        let failed = retain_last_success(&previous, failed);
        assert_eq!(failed.fetched_at_ms, Some(42));
        assert_eq!(failed.windows, previous.windows);
        assert!(failed.message.unwrap().contains("último dato"));
        let mut rejected = state("openrouter");
        rejected.status = ProviderUsageStatus::NeedsSignIn;
        assert!(retain_last_success(&previous, rejected).windows.is_empty());
        let mut unrelated = state("opencode");
        unrelated.status = ProviderUsageStatus::Error;
        assert!(retain_last_success(&previous, unrelated).windows.is_empty());
    }

    #[test]
    fn catalog_roster_uses_registered_credentials_and_supported_settings_accounts() {
        let registry = ProviderRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        for (id, _, _) in provider_connections::PROVIDERS {
            save_fixture(root.path(), id, true);
        }
        let connections = crate::provider_settings::connection_views_with_registry(
            &registry,
            &Default::default(),
            |_| false,
        );
        let initial = initial_snapshots(root.path(), &connections);
        assert_eq!(initial.len(), provider_connections::PROVIDERS.len());
        assert!(initial
            .values()
            .all(|state| state.status == ProviderUsageStatus::Disconnected));
        assert_eq!(
            initial["codex"].account_url.as_deref(),
            Some(provider_connections::CHATGPT_USAGE_URL)
        );
        assert_eq!(
            usage_url(registry.adapter("codex").unwrap()),
            Some("https://chatgpt.com/backend-api/wham/usage")
        );
        assert_eq!(usage_url(registry.adapter("nvidia").unwrap()), None);
        let mut go = registry.adapter("opencode").unwrap().clone();
        go.use_opencode_go();
        assert_eq!(usage_url(&go), Some("https://opencode.ai/zen/go/v1/usage"));
        assert_eq!(
            snapshot(&go, root.path(), true).plan_label.as_deref(),
            Some("OpenCode Go")
        );
    }

    #[test]
    fn initial_state_detects_credentials_changed_outside_the_connection_view() {
        let root = tempfile::tempdir().unwrap();
        let connections = crate::provider_settings::connection_views_with_registry(
            &ProviderRegistry::builtin(),
            &Default::default(),
            |_| false,
        );
        save_fixture(root.path(), "openrouter", false);
        assert!(!connections["openrouter"].key_available);
        assert_eq!(
            initial_snapshots(root.path(), &connections)["openrouter"].status,
            ProviderUsageStatus::Loading
        );
        save_fixture(root.path(), "openrouter", true);
        assert_eq!(
            initial_snapshots(root.path(), &connections)["openrouter"].status,
            ProviderUsageStatus::Disconnected
        );
    }

    #[test]
    fn fingerprints_change_with_credentials_and_go_service_without_exposing_secrets() {
        let mut adapter = ProviderRegistry::builtin()
            .adapter("opencode")
            .unwrap()
            .clone();
        let first = revision(&adapter, "placeholder-one");
        assert_ne!(first, revision(&adapter, "placeholder-two"));
        adapter.use_opencode_go();
        assert_ne!(first, revision(&adapter, "placeholder-one"));
    }

    #[test]
    fn http_uses_bearer_header_and_never_echoes_failure_bodies_or_follows_redirects() {
        for status in [401, 302, 500] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0; 4096];
                let count = socket.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
                assert!(request.starts_with("get /usage "));
                assert!(request.contains("authorization: bearer placeholder-key"));
                let body = "secret-error-body placeholder-key";
                write!(socket, "HTTP/1.1 {status} Error\r\nLocation: http://127.0.0.1:1/stolen\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let client = Client::builder().redirect(Policy::none()).build().unwrap();
            let adapter = ProviderRegistry::builtin()
                .adapter("openrouter")
                .unwrap()
                .clone();
            let error = query_usage(
                &client,
                &format!("http://{address}/usage"),
                "placeholder-key",
                None,
                &adapter,
                &mut state("openrouter"),
            )
            .err()
            .unwrap();
            assert!(!error.message.contains("placeholder-key"));
            assert!(!error.message.contains("secret-error-body"));
            assert_eq!(
                error.status,
                if status == 401 {
                    ProviderUsageStatus::NeedsSignIn
                } else {
                    ProviderUsageStatus::Error
                }
            );
            server.join().unwrap();
        }
    }
}
