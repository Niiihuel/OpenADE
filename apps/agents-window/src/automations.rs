//! Workspace-scoped, durable scheduled tasks.
//!
//! The UI owns drafts. This store owns definitions, due claims and the dispatch
//! ledger. It never executes a CLI itself: the host turns a claimed dispatch
//! into a persisted session and calls `complete_run` with that session ID.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::native_provider::NativeProviderSettings;

const VERSION: u32 = 1;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_AUTOMATIONS: usize = 20;
const MAX_RUNS_IN_LEDGER: usize = 256;
const MAX_LATE_MS: i64 = 5 * 60 * 1_000;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum AutomationError {
    Invalid(String),
    NotFound,
    LimitReached,
    Conflict(String),
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for AutomationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Conflict(message) => formatter.write_str(message),
            Self::NotFound => formatter.write_str("Automatización no encontrada"),
            Self::LimitReached => formatter.write_str("Límite de automatizaciones alcanzado"),
            Self::Io(error) => write!(formatter, "Error de almacenamiento: {error}"),
            Self::Json(error) => write!(formatter, "Datos de automatizaciones inválidos: {error}"),
        }
    }
}

impl std::error::Error for AutomationError {}

impl From<io::Error> for AutomationError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for AutomationError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Local wall-clock schedules use the operating system's time zone. An
/// interval remains anchored to its original instant, avoiding poll drift.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationSchedule {
    Once { at_ms: i64 },
    EveryMinutes { interval: u32, anchor_ms: i64 },
    Daily { hour: u8, minute: u8 },
    Weekly { weekday: u8, hour: u8, minute: u8 },
}

impl AutomationSchedule {
    pub fn validate(&self) -> Result<(), AutomationError> {
        let valid = match self {
            Self::Once { at_ms } => *at_ms > 0,
            Self::EveryMinutes {
                interval,
                anchor_ms,
            } => (1..=10_080).contains(interval) && *anchor_ms > 0,
            Self::Daily { hour, minute } => *hour < 24 && *minute < 60,
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => *weekday < 7 && *hour < 24 && *minute < 60,
        };
        if valid {
            Ok(())
        } else {
            Err(AutomationError::Invalid("Horario inválido".to_owned()))
        }
    }

    /// The first valid instant strictly after `after_ms`. Ambiguous local
    /// times (fall DST transition) resolve to the earliest remaining instant;
    /// nonexistent local times (spring transition) skip that occurrence.
    pub fn next_after(&self, after_ms: i64) -> Option<i64> {
        self.validate().ok()?;
        match self {
            Self::Once { at_ms } => (*at_ms > after_ms).then_some(*at_ms),
            Self::EveryMinutes {
                interval,
                anchor_ms,
            } => {
                let step = i64::from(*interval).checked_mul(60_000)?;
                let steps = after_ms
                    .saturating_sub(*anchor_ms)
                    .div_euclid(step)
                    .saturating_add(1)
                    .max(1);
                anchor_ms.checked_add(steps.checked_mul(step)?)
            }
            Self::Daily { hour, minute } | Self::Weekly { hour, minute, .. } => {
                let from = Local.timestamp_millis_opt(after_ms).single()?;
                let start = from.date_naive();
                for offset in 0..=15 {
                    let date = start.checked_add_signed(Duration::days(offset))?;
                    if let Self::Weekly { weekday, .. } = self {
                        if date.weekday().num_days_from_monday() != u32::from(*weekday) {
                            continue;
                        }
                    }
                    if let Some(candidate) = local_candidate(date, *hour, *minute, after_ms) {
                        return Some(candidate);
                    }
                }
                None
            }
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Once { at_ms } => Local
                .timestamp_millis_opt(*at_ms)
                .single()
                .map(|time| format!("Una vez · {}", time.format("%d/%m/%Y %H:%M")))
                .unwrap_or_else(|| "Una vez".to_owned()),
            Self::EveryMinutes { interval, .. } if *interval == 60 => "Cada hora".to_owned(),
            Self::EveryMinutes { interval, .. } => format!("Cada {interval} minutos"),
            Self::Daily { hour, minute } => format!("Diariamente a las {hour:02}:{minute:02}"),
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => {
                let day = [
                    "lunes",
                    "martes",
                    "miércoles",
                    "jueves",
                    "viernes",
                    "sábado",
                    "domingo",
                ][usize::from(*weekday)];
                format!("Cada {day} a las {hour:02}:{minute:02}")
            }
        }
    }
}

fn local_candidate(date: NaiveDate, hour: u8, minute: u8, after_ms: i64) -> Option<i64> {
    let local = date.and_hms_opt(u32::from(hour), u32::from(minute), 0)?;
    let candidates = match Local.from_local_datetime(&local) {
        LocalResult::None => return None,
        LocalResult::Single(value) => [Some(value.timestamp_millis()), None],
        LocalResult::Ambiguous(a, b) => [Some(a.timestamp_millis()), Some(b.timestamp_millis())],
    };
    candidates
        .into_iter()
        .flatten()
        .filter(|value| *value > after_ms)
        .min()
}

#[derive(Clone, Debug)]
pub struct AutomationDraft {
    pub title: String,
    pub prompt: String,
    pub backend: String,
    /// Frozen native connection choice. Credentials remain in the environment.
    pub native_provider: Option<NativeProviderSettings>,
    pub schedule: AutomationSchedule,
}

impl AutomationDraft {
    pub fn validate(&self) -> Result<(), AutomationError> {
        self.validate_structure()?;
        if self.backend.trim() == "native" {
            self.native_provider
                .as_ref()
                .ok_or_else(|| {
                    AutomationError::Invalid(
                        "Elige un proveedor y modelo nativo para esta automatización".to_owned(),
                    )
                })?
                .resolve()
                .map_err(AutomationError::Invalid)?;
        }
        Ok(())
    }

    /// Old stored native automations had no frozen selection. They must remain
    /// readable so the host can apply an explicit legacy fallback at dispatch.
    fn validate_structure(&self) -> Result<(), AutomationError> {
        let title = self.title.trim();
        let prompt = self.prompt.trim();
        let backend = self.backend.trim();
        if title.is_empty() || title.chars().count() > 100 {
            return Err(AutomationError::Invalid(
                "El título debe tener entre 1 y 100 caracteres".to_owned(),
            ));
        }
        if prompt.is_empty() || prompt.chars().count() > 10_000 {
            return Err(AutomationError::Invalid(
                "La instrucción debe tener entre 1 y 10 000 caracteres".to_owned(),
            ));
        }
        if backend.is_empty() || backend.len() > 64 || backend.contains(char::is_whitespace) {
            return Err(AutomationError::Invalid("Agente inválido".to_owned()));
        }
        self.schedule.validate()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Automation {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_provider: Option<NativeProviderSettings>,
    pub schedule: AutomationSchedule,
    pub enabled: bool,
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub run_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Pending,
    Dispatched,
    Failed,
    Skipped,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutomationRun {
    pub run_id: String,
    pub automation_id: String,
    pub scheduled_at: i64,
    pub manual: bool,
    pub status: AutomationRunStatus,
    pub session_id: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomationDispatch {
    pub run_id: String,
    pub automation_id: String,
    pub workspace_path: PathBuf,
    pub backend: String,
    pub native_provider: Option<NativeProviderSettings>,
    pub prompt: String,
    pub scheduled_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Document {
    version: u32,
    workspace_key: String,
    automations: Vec<Automation>,
    runs: Vec<AutomationRun>,
}

pub struct AutomationStore {
    path: PathBuf,
    workspace_path: PathBuf,
    document: Document,
}

impl AutomationStore {
    /// Discover all local workspace stores without stopping at one damaged
    /// document. Keep the returned stores alive as the sole writers for their
    /// respective files; do not reopen them on each scheduler tick.
    pub fn open_all(
        data_root: &Path,
    ) -> Result<Vec<Result<Self, AutomationError>>, AutomationError> {
        let directory = data_root.join("automations");
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut candidates = std::collections::BTreeSet::new();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let stem = name
                .strip_suffix(".json")
                .or_else(|| name.strip_suffix(".json.bak"));
            let Some(stem) = stem else { continue };
            if stem.len() == 16 && stem.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                candidates.insert(stem.to_owned());
            }
        }
        let mut stores = Vec::with_capacity(candidates.len());
        for stem in candidates {
            let primary = directory.join(format!("{stem}.json"));
            let path = if primary.exists() {
                primary
            } else {
                directory.join(format!("{stem}.json.bak"))
            };
            stores.push((|| -> Result<Self, AutomationError> {
                if fs::metadata(&path)?.len() > MAX_FILE_BYTES {
                    return Err(AutomationError::Invalid(format!(
                        "Archivo de automatizaciones demasiado grande: {stem}"
                    )));
                }
                let document: Document = serde_json::from_slice(&fs::read(&path)?)?;
                if document.version != VERSION
                    || format!("{:016x}", stable_hash(document.workspace_key.as_bytes())) != stem
                {
                    return Err(AutomationError::Invalid(format!(
                        "Archivo de automatizaciones inválido: {stem}"
                    )));
                }
                validate_document(&document)?;
                Self::open(data_root, Path::new(&document.workspace_key))
            })());
        }
        Ok(stores)
    }

    /// One file per canonical local workspace under `<data_root>/automations`.
    /// This type assumes one writer (the desktop application) for that file.
    pub fn open(data_root: &Path, workspace_path: &Path) -> Result<Self, AutomationError> {
        let workspace_path =
            fs::canonicalize(workspace_path).unwrap_or_else(|_| workspace_path.to_path_buf());
        if workspace_path.as_os_str().is_empty() {
            return Err(AutomationError::Invalid(
                "Selecciona un proyecto".to_owned(),
            ));
        }
        let key = workspace_key(&workspace_path);
        let path = data_root
            .join("automations")
            .join(format!("{:016x}.json", stable_hash(key.as_bytes())));
        let backup = path.with_extension("json.bak");
        if !path.exists() && backup.exists() {
            fs::rename(&backup, &path)?;
        }
        let document = if path.exists() {
            let metadata = fs::metadata(&path)?;
            if metadata.len() > MAX_FILE_BYTES {
                return Err(AutomationError::Invalid(
                    "Archivo de automatizaciones demasiado grande".to_owned(),
                ));
            }
            let loaded: Document = serde_json::from_slice(&fs::read(&path)?)?;
            if loaded.version != VERSION || loaded.workspace_key != key {
                return Err(AutomationError::Invalid(
                    "Datos de otro proyecto o versión incompatible".to_owned(),
                ));
            }
            validate_document(&loaded)?;
            loaded
        } else {
            Document {
                version: VERSION,
                workspace_key: key,
                automations: Vec::new(),
                runs: Vec::new(),
            }
        };
        let mut store = Self {
            path,
            workspace_path,
            document,
        };
        // An interrupted dispatch may already have created a session before
        // the process died. Do not automatically execute it a second time.
        if store
            .document
            .runs
            .iter()
            .any(|run| run.status == AutomationRunStatus::Pending)
        {
            let mut next = store.document.clone();
            for run in &mut next.runs {
                if run.status == AutomationRunStatus::Pending {
                    run.status = AutomationRunStatus::Interrupted;
                    run.error = Some(
                        "Ejecución interrumpida; revisa las sesiones antes de repetir".to_owned(),
                    );
                }
            }
            store.commit(next)?;
        }
        Ok(store)
    }

    pub fn workspace_path(&self) -> &Path {
        &self.workspace_path
    }

    pub fn automations(&self) -> &[Automation] {
        &self.document.automations
    }

    pub fn runs(&self) -> &[AutomationRun] {
        &self.document.runs
    }

    pub fn create(
        &mut self,
        draft: AutomationDraft,
        now_ms: i64,
    ) -> Result<String, AutomationError> {
        draft.validate()?;
        if self.document.automations.len() >= MAX_AUTOMATIONS {
            return Err(AutomationError::LimitReached);
        }
        let next_run_at = draft.schedule.next_after(now_ms.saturating_sub(1));
        if next_run_at.is_none() {
            return Err(AutomationError::Invalid(
                "El horario debe tener una ejecución futura".to_owned(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        let mut next = self.document.clone();
        next.automations.push(Automation {
            id: id.clone(),
            title: draft.title.trim().to_owned(),
            prompt: draft.prompt.trim().to_owned(),
            backend: draft.backend.trim().to_owned(),
            native_provider: draft.native_provider,
            schedule: draft.schedule,
            enabled: true,
            next_run_at,
            last_run_at: None,
            run_count: 0,
            created_at: now_ms,
            updated_at: now_ms,
        });
        self.commit(next)?;
        Ok(id)
    }

    pub fn update(
        &mut self,
        id: &str,
        draft: AutomationDraft,
        now_ms: i64,
    ) -> Result<(), AutomationError> {
        draft.validate()?;
        let mut next = self.document.clone();
        let automation = next
            .automations
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(AutomationError::NotFound)?;
        let next_run_at = draft.schedule.next_after(now_ms.saturating_sub(1));
        if next_run_at.is_none() {
            return Err(AutomationError::Invalid(
                "El horario debe tener una ejecución futura".to_owned(),
            ));
        }
        automation.title = draft.title.trim().to_owned();
        automation.prompt = draft.prompt.trim().to_owned();
        automation.backend = draft.backend.trim().to_owned();
        automation.native_provider = draft.native_provider;
        automation.schedule = draft.schedule;
        automation.next_run_at = next_run_at;
        automation.updated_at = now_ms;
        self.commit(next)
    }

    pub fn set_enabled(
        &mut self,
        id: &str,
        enabled: bool,
        now_ms: i64,
    ) -> Result<(), AutomationError> {
        let mut next = self.document.clone();
        let automation = next
            .automations
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(AutomationError::NotFound)?;
        if automation.enabled == enabled {
            return Ok(());
        }
        if enabled {
            automation.next_run_at = automation.schedule.next_after(now_ms);
            if automation.next_run_at.is_none() {
                return Err(AutomationError::Invalid(
                    "La ejecución única ya venció; edita su horario".to_owned(),
                ));
            }
        }
        automation.enabled = enabled;
        automation.updated_at = now_ms;
        self.commit(next)
    }

    pub fn delete(&mut self, id: &str) -> Result<(), AutomationError> {
        let mut next = self.document.clone();
        let before = next.automations.len();
        next.automations.retain(|item| item.id != id);
        if next.automations.len() == before {
            return Err(AutomationError::NotFound);
        }
        // Keep the bounded history as an audit trail. New dispatch is blocked
        // because there is no longer a definition.
        self.commit(next)
    }

    /// Persist every claim before returning it. At most one scheduled claim
    /// per definition is produced on a tick, even after a long sleep.
    pub fn claim_due(&mut self, now_ms: i64) -> Result<Vec<AutomationDispatch>, AutomationError> {
        let mut next = self.document.clone();
        let mut claims = Vec::new();
        for automation in &mut next.automations {
            let Some(due) = automation.next_run_at else {
                continue;
            };
            if !automation.enabled || due > now_ms {
                continue;
            }
            let run_id = format!("{}:{due}", automation.id);
            let late = now_ms.saturating_sub(due) > MAX_LATE_MS;
            if !next.runs.iter().any(|run| run.run_id == run_id) {
                next.runs.push(AutomationRun {
                    run_id: run_id.clone(),
                    automation_id: automation.id.clone(),
                    scheduled_at: due,
                    manual: false,
                    status: if late {
                        AutomationRunStatus::Skipped
                    } else {
                        AutomationRunStatus::Pending
                    },
                    session_id: None,
                    error: late
                        .then(|| "La aplicación estuvo cerrada durante esta ejecución".to_owned()),
                    created_at: now_ms,
                    updated_at: now_ms,
                });
                if !late {
                    claims.push(dispatch_for(&self.workspace_path, automation, run_id, due));
                }
                automation.run_count = automation.run_count.saturating_add(1);
                automation.last_run_at = Some(due);
            }
            automation.next_run_at = automation.schedule.next_after(now_ms);
            if automation.next_run_at.is_none() {
                automation.enabled = false;
            }
            automation.updated_at = now_ms;
        }
        if next.runs == self.document.runs && next.automations == self.document.automations {
            return Ok(Vec::new());
        }
        trim_runs(&mut next);
        self.commit(next)?;
        Ok(claims)
    }

    pub fn claim_manual(
        &mut self,
        id: &str,
        now_ms: i64,
    ) -> Result<AutomationDispatch, AutomationError> {
        let automation = self
            .document
            .automations
            .iter()
            .find(|item| item.id == id)
            .ok_or(AutomationError::NotFound)?
            .clone();
        let run_id = format!("{}:manual:{}", id, Uuid::new_v4());
        let mut next = self.document.clone();
        next.runs.push(AutomationRun {
            run_id: run_id.clone(),
            automation_id: id.to_owned(),
            scheduled_at: now_ms,
            manual: true,
            status: AutomationRunStatus::Pending,
            session_id: None,
            error: None,
            created_at: now_ms,
            updated_at: now_ms,
        });
        trim_runs(&mut next);
        self.commit(next)?;
        Ok(dispatch_for(
            &self.workspace_path,
            &automation,
            run_id,
            now_ms,
        ))
    }

    /// `result` reports dispatch to a persistent session, not completion of
    /// the agent turn. The session ID allows navigation and later diagnostics.
    pub fn complete_run(
        &mut self,
        run_id: &str,
        result: Result<String, String>,
        now_ms: i64,
    ) -> Result<(), AutomationError> {
        let mut next = self.document.clone();
        let run = next
            .runs
            .iter_mut()
            .find(|item| item.run_id == run_id)
            .ok_or(AutomationError::NotFound)?;
        let (status, session_id, error) = match result {
            Ok(session_id) if !session_id.trim().is_empty() => {
                (AutomationRunStatus::Dispatched, Some(session_id), None)
            }
            Ok(_) => {
                return Err(AutomationError::Invalid(
                    "Falta el ID de la sesión".to_owned(),
                ))
            }
            Err(message) => (
                AutomationRunStatus::Failed,
                None,
                Some(message.chars().take(500).collect()),
            ),
        };
        if run.status != AutomationRunStatus::Pending {
            if run.status == status && run.session_id == session_id && run.error == error {
                return Ok(());
            }
            return Err(AutomationError::Conflict(
                "Esta ejecución ya fue finalizada".to_owned(),
            ));
        }
        run.status = status;
        run.session_id = session_id;
        run.error = error;
        run.updated_at = now_ms;
        self.commit(next)
    }

    fn commit(&mut self, next: Document) -> Result<(), AutomationError> {
        validate_document(&next)?;
        let payload = serde_json::to_vec_pretty(&next)?;
        if payload.len() as u64 > MAX_FILE_BYTES {
            return Err(AutomationError::Invalid(
                "Límite de historial alcanzado".to_owned(),
            ));
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| AutomationError::Invalid("Ruta de datos inválida".to_owned()))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".automations-{}-{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&payload)?;
            file.sync_all()?;
            if let Err(first_error) = fs::rename(&temporary, &self.path) {
                if !self.path.exists() {
                    return Err(first_error);
                }
                // Windows does not consistently replace an existing file via
                // rename. Preserve the old copy until the new file is in place.
                let backup = self.path.with_extension("json.bak");
                if backup.exists() {
                    fs::remove_file(&backup)?;
                }
                fs::rename(&self.path, &backup)?;
                if let Err(error) = fs::rename(&temporary, &self.path) {
                    let _ = fs::rename(&backup, &self.path);
                    return Err(error);
                }
                let _ = fs::remove_file(backup);
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        self.document = next;
        Ok(())
    }
}

fn dispatch_for(
    workspace_path: &Path,
    automation: &Automation,
    run_id: String,
    scheduled_at: i64,
) -> AutomationDispatch {
    AutomationDispatch {
        run_id,
        automation_id: automation.id.clone(),
        workspace_path: workspace_path.to_path_buf(),
        backend: automation.backend.clone(),
        native_provider: automation.native_provider.clone(),
        prompt: automation.prompt.clone(),
        scheduled_at,
    }
}

fn trim_runs(document: &mut Document) {
    if document.runs.len() <= MAX_RUNS_IN_LEDGER {
        return;
    }
    let excess = document.runs.len() - MAX_RUNS_IN_LEDGER;
    document.runs.drain(0..excess);
}

fn validate_document(document: &Document) -> Result<(), AutomationError> {
    if document.automations.len() > MAX_AUTOMATIONS || document.runs.len() > MAX_RUNS_IN_LEDGER {
        return Err(AutomationError::Invalid(
            "Demasiados datos de automatizaciones".to_owned(),
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for automation in &document.automations {
        if !ids.insert(&automation.id) || automation.id.len() > 64 {
            return Err(AutomationError::Invalid(
                "ID de automatización duplicado o inválido".to_owned(),
            ));
        }
        AutomationDraft {
            title: automation.title.clone(),
            prompt: automation.prompt.clone(),
            backend: automation.backend.clone(),
            native_provider: automation.native_provider.clone(),
            schedule: automation.schedule.clone(),
        }
        .validate_structure()?;
    }
    let mut run_ids = std::collections::HashSet::new();
    for run in &document.runs {
        if !run_ids.insert(&run.run_id) || run.run_id.len() > 150 {
            return Err(AutomationError::Invalid(
                "ID de ejecución duplicado o inválido".to_owned(),
            ));
        }
    }
    Ok(())
}

fn workspace_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

/// Parse a user-entered local date and time from the editor. DST gaps and
/// ambiguous times are rejected so the user can choose an unambiguous value.
pub fn parse_local_once(value: &str) -> Result<i64, AutomationError> {
    let naive = NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M")
        .map_err(|_| AutomationError::Invalid("Usa AAAA-MM-DD HH:MM".to_owned()))?;
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(date) => Ok(date.timestamp_millis()),
        LocalResult::None => Err(AutomationError::Invalid(
            "Esta hora no existe en tu zona horaria".to_owned(),
        )),
        LocalResult::Ambiguous(_, _) => Err(AutomationError::Invalid(
            "Esta hora es ambigua en tu zona horaria".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(schedule: AutomationSchedule) -> AutomationDraft {
        AutomationDraft {
            title: "Informe diario".to_owned(),
            prompt: "Resume los cambios del proyecto".to_owned(),
            backend: "codex".to_owned(),
            native_provider: None,
            schedule,
        }
    }

    fn native_settings(model_id: &str) -> NativeProviderSettings {
        NativeProviderSettings {
            provider_id: "openai".to_owned(),
            model_id: model_id.to_owned(),
            custom_endpoint: "https://api.openai.com/v1".to_owned(),
        }
    }

    #[test]
    fn native_automation_freezes_provider_and_model_without_credentials() {
        let root = tempfile::tempdir().unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let mut first = draft(AutomationSchedule::Once { at_ms: 2_000_000 });
        first.backend = "native".to_owned();
        assert!(matches!(first.validate(), Err(AutomationError::Invalid(_))));
        first.native_provider = Some(native_settings("gpt-4.1"));
        let id = store.create(first, 1_000_000).unwrap();
        assert_eq!(
            store.automations()[0].native_provider,
            Some(native_settings("gpt-4.1"))
        );

        let mut changed = draft(AutomationSchedule::Once { at_ms: 2_000_000 });
        changed.backend = "native".to_owned();
        changed.native_provider = Some(native_settings("gpt-6-sol"));
        store.update(&id, changed, 1_000_100).unwrap();
        drop(store);

        let mut reopened = AutomationStore::open(root.path(), root.path()).unwrap();
        let serialized = serde_json::to_value(&reopened.automations()[0]).unwrap();
        assert!(serialized.get("native_provider").is_some());
        assert!(serialized["native_provider"].get("api_key").is_none());
        let dispatch = reopened.claim_due(2_000_000).unwrap().remove(0);
        assert_eq!(dispatch.native_provider, Some(native_settings("gpt-6-sol")));
    }

    #[test]
    fn old_native_automation_without_snapshot_remains_readable() {
        let root = tempfile::tempdir().unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let mut native = draft(AutomationSchedule::Once { at_ms: 2_000_000 });
        native.backend = "native".to_owned();
        native.native_provider = Some(native_settings("gpt-4.1"));
        store.create(native, 1_000_000).unwrap();
        let path = store.path.clone();
        drop(store);

        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        document["automations"][0]
            .as_object_mut()
            .unwrap()
            .remove("native_provider");
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

        let mut reopened = AutomationStore::open(root.path(), root.path()).unwrap();
        assert_eq!(reopened.automations()[0].native_provider, None);
        assert_eq!(
            reopened.claim_due(2_000_000).unwrap()[0].native_provider,
            None
        );
    }

    #[test]
    fn anchored_interval_does_not_drift_after_late_poll() {
        let schedule = AutomationSchedule::EveryMinutes {
            interval: 10,
            anchor_ms: 1_000_000,
        };
        assert_eq!(schedule.next_after(1_000_000), Some(1_600_000));
        assert_eq!(schedule.next_after(1_630_000), Some(2_200_000));
    }

    #[test]
    fn claims_once_and_persists_dispatch_session() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        fs::create_dir(&workspace).unwrap();
        let mut store = AutomationStore::open(root.path(), &workspace).unwrap();
        let id = store
            .create(
                draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        assert!(store.claim_due(1_999_999).unwrap().is_empty());
        let claims = store.claim_due(2_000_000).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].automation_id, id);
        assert!(store.claim_due(2_000_000).unwrap().is_empty());
        store
            .complete_run(&claims[0].run_id, Ok("session-123".to_owned()), 2_000_010)
            .unwrap();
        drop(store);
        let store = AutomationStore::open(root.path(), &workspace).unwrap();
        assert_eq!(store.runs()[0].session_id.as_deref(), Some("session-123"));
        assert!(!store.automations()[0].enabled);
    }

    #[test]
    fn crash_during_dispatch_marks_interrupted_without_duplicate() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        fs::create_dir(&workspace).unwrap();
        let mut store = AutomationStore::open(root.path(), &workspace).unwrap();
        store
            .create(
                draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        let claim = store.claim_due(2_000_000).unwrap().remove(0);
        drop(store);
        let mut store = AutomationStore::open(root.path(), &workspace).unwrap();
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Interrupted);
        assert!(store.claim_due(2_000_000).unwrap().is_empty());
        assert!(store
            .complete_run(&claim.run_id, Ok("x".to_owned()), 2_000_000)
            .is_err());
    }

    #[test]
    fn pause_edit_delete_and_manual_run_follow_workspace_scope() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        let mut store = AutomationStore::open(root.path(), &a).unwrap();
        let id = store
            .create(
                draft(AutomationSchedule::EveryMinutes {
                    interval: 5,
                    anchor_ms: 1_000_000,
                }),
                1_000_000,
            )
            .unwrap();
        store.set_enabled(&id, false, 1_100_000).unwrap();
        assert!(store.claim_due(1_300_000).unwrap().is_empty());
        let manual = store.claim_manual(&id, 1_310_000).unwrap();
        assert_eq!(manual.workspace_path, fs::canonicalize(&a).unwrap());
        store
            .complete_run(
                &manual.run_id,
                Err("CLI no disponible".to_owned()),
                1_310_001,
            )
            .unwrap();
        store
            .update(
                &id,
                draft(AutomationSchedule::Daily { hour: 9, minute: 0 }),
                1_320_000,
            )
            .unwrap();
        store.set_enabled(&id, true, 1_320_000).unwrap();
        store.delete(&id).unwrap();
        assert!(store.automations().is_empty());
        assert_eq!(store.runs().len(), 1);
        assert!(AutomationStore::open(root.path(), &b)
            .unwrap()
            .automations()
            .is_empty());
    }

    #[test]
    fn missed_run_is_skipped_instead_of_surprising_late_execution() {
        let root = tempfile::tempdir().unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        store
            .create(
                draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        assert!(store
            .claim_due(2_000_000 + MAX_LATE_MS + 1)
            .unwrap()
            .is_empty());
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Skipped);
    }

    #[test]
    fn invalid_definition_does_not_mutate_saved_data() {
        let root = tempfile::tempdir().unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let valid = draft(AutomationSchedule::Daily {
            hour: 9,
            minute: 30,
        });
        let id = store.create(valid, 1_000_000).unwrap();
        let before = store.automations()[0].clone();
        let mut invalid = draft(AutomationSchedule::Daily {
            hour: 32,
            minute: 30,
        });
        invalid.title.clear();
        assert!(store.update(&id, invalid, 1_100_000).is_err());
        assert_eq!(store.automations()[0], before);
    }

    #[test]
    fn open_all_discovers_workspaces_and_isolates_corrupt_documents() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data");
        let first_workspace = root.path().join("first");
        let second_workspace = root.path().join("second");
        fs::create_dir(&first_workspace).unwrap();
        fs::create_dir(&second_workspace).unwrap();
        let mut first = AutomationStore::open(&data_root, &first_workspace).unwrap();
        let mut second = AutomationStore::open(&data_root, &second_workspace).unwrap();
        first
            .create(
                draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        second
            .create(
                draft(AutomationSchedule::Once { at_ms: 3_000_000 }),
                1_000_000,
            )
            .unwrap();
        drop(first);
        drop(second);
        fs::write(
            data_root.join("automations").join("0000000000000000.json"),
            b"not json",
        )
        .unwrap();
        let discovered = AutomationStore::open_all(&data_root).unwrap();
        assert_eq!(discovered.len(), 3);
        assert_eq!(discovered.iter().filter(|store| store.is_ok()).count(), 2);
        assert_eq!(discovered.iter().filter(|store| store.is_err()).count(), 1);
        let paths: Vec<_> = discovered
            .into_iter()
            .filter_map(Result::ok)
            .map(|store| store.workspace_path().to_path_buf())
            .collect();
        assert!(paths.contains(&fs::canonicalize(first_workspace).unwrap()));
        assert!(paths.contains(&fs::canonicalize(second_workspace).unwrap()));
    }

    #[test]
    fn open_all_recovers_backup_when_primary_was_not_installed() {
        let root = tempfile::tempdir().unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        store
            .create(
                draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        let primary = store.path.clone();
        drop(store);
        let backup = primary.with_extension("json.bak");
        fs::rename(&primary, &backup).unwrap();
        let discovered = AutomationStore::open_all(root.path()).unwrap();
        assert_eq!(discovered.len(), 1);
        assert!(discovered[0].is_ok());
        assert!(primary.exists());
        assert!(!backup.exists());
    }
}
