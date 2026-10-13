//! Workspace-scoped, durable scheduled tasks.
//!
//! The UI owns drafts. This store owns definitions, due claims and the dispatch
//! ledger. It never executes a CLI itself: the host turns a claimed dispatch
//! into a persisted session and calls `complete_run` with that session ID.
//! Native recovery can attach immutable session provenance without confirming
//! dispatch. Native results are separate, verified projections of the original
//! session and journal. This bounded ledger never grants execution authority.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use openide_core::{Session, StoredSession};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    native_automation_result::{NativeAutomationResultV1, ValidatedNativeAutomationResult},
    native_provider::NativeProviderSettings,
};

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
            Self::NotFound => formatter.write_str("Automatización o ejecución no encontrada"),
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
    /// Several days of the week: bit 0 is Monday, bit 6 Sunday.
    Weekdays { days: u8, hour: u8, minute: u8 },
    /// A day of every month; a month without that day is passed over.
    Monthly { day: u8, hour: u8, minute: u8 },
}

const DAY_NAMES: [&str; 7] = ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"];
/// Monday to Friday, as [`AutomationSchedule::Weekdays`] counts them.
pub const WORKDAYS: u8 = 0b001_1111;

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
            Self::Weekdays { days, hour, minute } => *days != 0 && *days < 128 && *hour < 24 && *minute < 60,
            Self::Monthly { day, hour, minute } => (1..=31).contains(day) && *hour < 24 && *minute < 60,
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
            Self::Daily { hour, minute }
            | Self::Weekly { hour, minute, .. }
            | Self::Weekdays { hour, minute, .. }
            | Self::Monthly { hour, minute, .. } => {
                let from = Local.timestamp_millis_opt(after_ms).single()?;
                let start = from.date_naive();
                // Far enough for a 31st, which two months in a row may lack.
                for offset in 0..=95 {
                    let date = start.checked_add_signed(Duration::days(offset))?;
                    let weekday = date.weekday().num_days_from_monday();
                    let wanted = match self {
                        Self::Weekly { weekday: day, .. } => weekday == u32::from(*day),
                        Self::Weekdays { days, .. } => days & (1 << weekday) != 0,
                        Self::Monthly { day, .. } => date.day() == u32::from(*day),
                        _ => true,
                    };
                    if !wanted {
                        continue;
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
                let day = DAY_NAMES[usize::from(*weekday)];
                format!("Cada {day} a las {hour:02}:{minute:02}")
            }
            Self::Weekdays { days, hour, minute } if *days == WORKDAYS => {
                format!("Días hábiles a las {hour:02}:{minute:02}")
            }
            Self::Weekdays { days, hour, minute } => {
                let named: Vec<&str> = (0..7).filter(|day| days & (1 << day) != 0).map(|day| DAY_NAMES[day]).collect();
                format!("Cada {} a las {hour:02}:{minute:02}", named.join(", "))
            }
            Self::Monthly { day, hour, minute } => format!("El día {day} de cada mes a las {hour:02}:{minute:02}"),
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
        if self.backend.trim() == "native" && self.native_provider.is_none() {
            return Err(AutomationError::Invalid(
                "Elige un proveedor y modelo nativo para esta automatización".to_owned(),
            ));
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
        if let Some(selection) = self
            .native_provider
            .as_ref()
            .filter(|_| backend == "native")
        {
            // Validate only the stored choice's shape. Registry discovery,
            // exact model capabilities, endpoint and credentials are checked
            // by the host against data_root before native admission.
            if selection.provider_id.trim().is_empty()
                || selection.provider_id.len() > 64
                || !selection.provider_id.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'
                        || byte == b'_'
                })
            {
                return Err(AutomationError::Invalid(
                    "ID de proveedor nativo inválido".into(),
                ));
            }
            if selection.model_id.trim().is_empty()
                || selection.model_id.len() > openide_core::MAX_MODEL_ROUTE_MODEL_BYTES
                || selection.model_id.chars().any(char::is_control)
            {
                return Err(AutomationError::Invalid(
                    "ID de modelo nativo inválido".into(),
                ));
            }
            if selection.custom_endpoint.len() > openide_core::MAX_MODEL_ROUTE_ENDPOINT_BYTES
                || selection.custom_endpoint.chars().any(char::is_control)
            {
                return Err(AutomationError::Invalid("Endpoint nativo inválido".into()));
            }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_result: Option<NativeAutomationResultV1>,
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
    /// The task's name, which the session of the run takes.
    pub title: String,
    pub scheduled_at: i64,
    /// The persisted claim's origin; never inferred from the run ID format.
    pub manual: bool,
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
                        "Ejecución interrumpida; revisa las sesiones. No se repite automáticamente"
                            .to_owned(),
                    );
                }
            }
            store.commit(next)?;
        }
        // Runs left behind by tasks deleted before their history went with them.
        let known = |run: &AutomationRun| store.document.automations.iter().any(|item| item.id == run.automation_id);
        if !store.document.runs.iter().all(known) {
            let mut next = store.document.clone();
            next.runs.retain(|run| next.automations.iter().any(|item| item.id == run.automation_id));
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
        let mut draft = draft;
        // The same interval keeps its cadence: an edit of the name does not
        // start the count again.
        if let (
            AutomationSchedule::EveryMinutes { interval, anchor_ms },
            AutomationSchedule::EveryMinutes { interval: kept, anchor_ms: kept_anchor },
        ) = (&mut draft.schedule, &automation.schedule)
        {
            if interval == kept {
                *anchor_ms = *kept_anchor;
            }
        }
        // A schedule left as it was keeps its next run, even when it has none.
        let next_run_at = if draft.schedule == automation.schedule {
            automation.next_run_at
        } else {
            let next = draft.schedule.next_after(now_ms.saturating_sub(1));
            if next.is_none() {
                return Err(AutomationError::Invalid(
                    "El horario debe tener una ejecución futura".to_owned(),
                ));
            }
            next
        };
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
        // Its history goes with it: nothing shows the runs of a task that is
        // gone, and they would take room from those of the tasks that stay.
        // Each run's conversation is a session of its own and remains.
        next.runs.retain(|run| run.automation_id != id);
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
                    native_result: None,
                    error: late
                        .then(|| "La aplicación estuvo cerrada durante esta ejecución".to_owned()),
                    created_at: now_ms,
                    updated_at: now_ms,
                });
                // A run that was skipped did not happen: it is in the
                // history, not in the count.
                if !late {
                    claims.push(dispatch_for(
                        &self.workspace_path,
                        automation,
                        run_id,
                        due,
                        false,
                    ));
                    automation.run_count = automation.run_count.saturating_add(1);
                    automation.last_run_at = Some(due);
                }
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
            native_result: None,
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
            true,
        ))
    }

    /// Link a session whose immutable native origin proves this existing claim.
    /// This never creates a claim or dispatches work, and preserves Pending or
    /// Interrupted: a saved session is not proof that the worker was started.
    /// Returns false for an already identical link, without rewriting the file.
    /// The host supplies metadata created/loaded through SessionStore and must
    /// separately validate its authority JSON before executing or resuming it.
    pub fn attach_existing_native_session(
        &mut self,
        run_id: &str,
        session: &Session,
        now_ms: i64,
    ) -> Result<bool, AutomationError> {
        let index = self
            .document
            .runs
            .iter()
            .position(|run| run.run_id == run_id)
            .ok_or(AutomationError::NotFound)?;
        let run = &self.document.runs[index];
        if !matches!(
            run.status,
            AutomationRunStatus::Pending
                | AutomationRunStatus::Interrupted
                | AutomationRunStatus::Dispatched
        ) {
            return Err(AutomationError::Conflict(
                "Esta ejecución no admite recuperar un enlace de sesión".into(),
            ));
        }
        let origin = session.native_automation.as_ref().ok_or_else(|| {
            AutomationError::Invalid("La sesión no tiene un origen de automatización nativa".into())
        })?;
        origin
            .validate_session(session)
            .map_err(|error| AutomationError::Invalid(error.to_string()))?;
        if session.workspace != self.workspace_path
            || origin.automation_id != run.automation_id
            || origin.run_id != run.run_id
            || origin.scheduled_at_ms != run.scheduled_at
            || origin.manual != run.manual
        {
            return Err(AutomationError::Conflict(
                "El origen de la sesión no corresponde a esta ejecución y proyecto".into(),
            ));
        }
        if let Some(saved) = &run.session_id {
            return if saved == &session.id {
                Ok(false)
            } else {
                Err(AutomationError::Conflict(
                    "Esta ejecución ya está vinculada a otra sesión".into(),
                ))
            };
        }
        let mut next = self.document.clone();
        next.runs[index].session_id = Some(session.id.clone());
        next.runs[index].updated_at = now_ms;
        self.commit(next)?;
        Ok(true)
    }

    /// Save a verified native journal projection without changing dispatch
    /// status. A claimed run must still exist, and a newer observation must
    /// retain the complete previously observed prefix and terminal outcome.
    pub fn record_native_result(
        &mut self,
        record: &StoredSession,
        result: &ValidatedNativeAutomationResult,
        now_ms: i64,
    ) -> Result<bool, AutomationError> {
        result
            .verify_record(record)
            .map_err(AutomationError::Invalid)?;
        let index = self
            .document
            .runs
            .iter()
            .position(|run| run.run_id == result.run_id())
            .ok_or(AutomationError::NotFound)?;
        let run = &self.document.runs[index];
        let origin = record.session.native_automation.as_ref().ok_or_else(|| {
            AutomationError::Invalid("La sesión no tiene un origen de automatización nativa".into())
        })?;
        origin
            .validate_session(&record.session)
            .map_err(|error| AutomationError::Invalid(error.to_string()))?;
        if run.status == AutomationRunStatus::Skipped
            || record.session.workspace != self.workspace_path
            || result.workspace() != self.workspace_path
            || origin.automation_id != run.automation_id
            || origin.run_id != run.run_id
            || origin.scheduled_at_ms != run.scheduled_at
            || origin.manual != run.manual
            || result.automation_id() != run.automation_id
            || result.scheduled_at_ms() != run.scheduled_at
            || result.manual() != run.manual
            || result.session_id() != record.session.id
        {
            return Err(AutomationError::Conflict(
                "El resultado no corresponde a esta ejecución y proyecto".into(),
            ));
        }
        if run
            .session_id
            .as_deref()
            .is_some_and(|saved| saved != result.session_id())
        {
            return Err(AutomationError::Conflict(
                "Esta ejecución ya está vinculada a otra sesión".into(),
            ));
        }
        if let Some(previous) = &run.native_result {
            if result.last_sequence() < previous.last_sequence() {
                return Err(AutomationError::Conflict(
                    "El resultado es anterior al journal ya registrado".into(),
                ));
            }
            if result.last_sequence() == previous.last_sequence() {
                return if previous == result.snapshot() {
                    Ok(false)
                } else {
                    Err(AutomationError::Conflict(
                        "El resultado cambió una observación ya registrada".into(),
                    ))
                };
            }
            previous
                .verify_record_prefix(record)
                .map_err(AutomationError::Conflict)?;
            if previous.terminal_sequence().is_some()
                && (previous.terminal_sequence() != result.terminal_sequence()
                    || previous.outcome() != result.outcome())
            {
                return Err(AutomationError::Conflict(
                    "El resultado intenta reemplazar el terminal registrado".into(),
                ));
            }
        }
        let mut next = self.document.clone();
        next.runs[index].session_id = Some(record.session.id.clone());
        next.runs[index].native_result = Some(result.snapshot().clone());
        next.runs[index].updated_at = now_ms;
        self.commit(next)?;
        Ok(true)
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
                if run
                    .session_id
                    .as_ref()
                    .is_some_and(|saved| saved != &session_id)
                {
                    return Err(AutomationError::Conflict(
                        "El despacho intenta reemplazar la sesión ya vinculada".into(),
                    ));
                }
                (AutomationRunStatus::Dispatched, Some(session_id), None)
            }
            Ok(_) => {
                return Err(AutomationError::Invalid(
                    "Falta el ID de la sesión".to_owned(),
                ))
            }
            Err(message) => (
                AutomationRunStatus::Failed,
                run.session_id.clone(),
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
            #[cfg(unix)]
            fs::File::open(parent)?.sync_all()?;
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
    manual: bool,
) -> AutomationDispatch {
    AutomationDispatch {
        run_id,
        automation_id: automation.id.clone(),
        workspace_path: workspace_path.to_path_buf(),
        backend: automation.backend.clone(),
        native_provider: automation.native_provider.clone(),
        prompt: automation.prompt.clone(),
        title: automation.title.clone(),
        scheduled_at,
        manual,
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
        if let Some(result) = &run.native_result {
            result.validate().map_err(AutomationError::Invalid)?;
            if run.status == AutomationRunStatus::Skipped
                || run.session_id.as_deref() != Some(result.session_id())
                || run.automation_id != result.automation_id()
                || run.run_id != result.run_id()
                || run.scheduled_at != result.scheduled_at_ms()
                || run.manual != result.manual()
                || workspace_key(result.workspace()) != document.workspace_key
            {
                return Err(AutomationError::Invalid(
                    "El resultado nativo no corresponde al enlace de la ejecución".into(),
                ));
            }
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
pub(crate) mod tests {
    use super::*;
    use crate::native_automation::FrozenNativeAutomationAuthorityV1;
    use openide_core::{
        AgentEvent, AgentEventKind, CheckpointFileStore, NativeAutomationOrigin, SessionStore,
        TurnMode,
    };

    fn draft(schedule: AutomationSchedule) -> AutomationDraft {
        AutomationDraft {
            title: "Informe diario".to_owned(),
            prompt: "Resume los cambios del proyecto".to_owned(),
            backend: "codex".to_owned(),
            native_provider: None,
            schedule,
        }
    }

    #[test]
    fn several_days_of_the_week_and_a_day_of_the_month_find_their_next_run() {
        use chrono::{Datelike, TimeZone, Timelike};
        // A Wednesday at noon, local time.
        let from = Local.with_ymd_and_hms(2026, 1, 7, 12, 0, 0).single().unwrap().timestamp_millis();
        let at = |ms: i64| Local.timestamp_millis_opt(ms).single().unwrap();
        let work = AutomationSchedule::Weekdays { days: WORKDAYS, hour: 9, minute: 30 };
        let next = at(work.next_after(from).unwrap());
        assert_eq!((next.day(), next.hour(), next.minute()), (8, 9, 30));
        // From Friday after the hour, the weekend is passed over.
        let friday = Local.with_ymd_and_hms(2026, 1, 9, 10, 0, 0).single().unwrap().timestamp_millis();
        assert_eq!(at(work.next_after(friday).unwrap()).day(), 12);
        assert_eq!(work.summary(), "Días hábiles a las 09:30");
        let two = AutomationSchedule::Weekdays { days: 0b100_0001, hour: 8, minute: 0 };
        assert_eq!(two.summary(), "Cada lunes, domingo a las 08:00");
        assert_eq!(at(two.next_after(from).unwrap()).day(), 11);
        // A month without the day is passed over, not moved.
        let last = AutomationSchedule::Monthly { day: 31, hour: 7, minute: 0 };
        let february = Local.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).single().unwrap().timestamp_millis();
        let next = at(last.next_after(february).unwrap());
        assert_eq!((next.month(), next.day()), (3, 31));
        for invalid in [
            AutomationSchedule::Weekdays { days: 0, hour: 9, minute: 0 },
            AutomationSchedule::Weekdays { days: 128, hour: 9, minute: 0 },
            AutomationSchedule::Monthly { day: 0, hour: 9, minute: 0 },
            AutomationSchedule::Monthly { day: 32, hour: 9, minute: 0 },
        ] {
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn a_deleted_task_takes_its_history_and_one_left_behind_goes_when_the_project_opens() {
        let root = tempfile::tempdir().unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let gone = store.create(draft(AutomationSchedule::Once { at_ms: 2_000_000 }), 1_000_000).unwrap();
        let kept = store.create(draft(AutomationSchedule::Once { at_ms: 2_000_000 }), 1_000_000).unwrap();
        assert_eq!(store.claim_due(2_000_000).unwrap().len(), 2);
        store.delete(&gone).unwrap();
        assert_eq!(store.runs().iter().map(|run| run.automation_id.as_str()).collect::<Vec<_>>(), [kept.as_str()]);
        // A file written before this, with the runs of a task that is gone.
        let mut orphaned = store.document.clone();
        let mut run = orphaned.runs[0].clone();
        run.run_id = "antes:1".into();
        run.automation_id = "antes".into();
        orphaned.runs.push(run);
        store.commit(orphaned).unwrap();
        let reopened = AutomationStore::open(root.path(), root.path()).unwrap();
        assert_eq!(reopened.runs().iter().map(|run| run.automation_id.as_str()).collect::<Vec<_>>(), [kept.as_str()]);
    }

    #[test]
    fn renaming_keeps_the_cadence_and_a_one_off_that_ran_can_still_be_renamed() {
        let root = tempfile::tempdir().unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let every = |anchor_ms| AutomationSchedule::EveryMinutes { interval: 10, anchor_ms };
        let id = store.create(draft(every(1_000_000)), 1_000_000).unwrap();
        let next = store.automations()[0].next_run_at;
        let mut renamed = draft(every(1_300_000));
        renamed.title = "Otro nombre".into();
        store.update(&id, renamed, 1_300_000).unwrap();
        assert_eq!((store.automations()[0].title.as_str(), store.automations()[0].next_run_at), ("Otro nombre", next));
        assert_eq!(store.automations()[0].schedule, every(1_000_000));

        let once = store.create(draft(AutomationSchedule::Once { at_ms: 2_000_000 }), 1_000_000).unwrap();
        let claimed = store.claim_due(2_000_000).unwrap();
        // A run that was claimed counts; one that came too late would not.
        assert_eq!((claimed.len(), claimed[0].title.as_str()), (1, "Informe diario"));
        let ran = store.automations().iter().find(|item| item.id == once).unwrap().clone();
        assert_eq!((ran.run_count, ran.next_run_at), (1, None));
        let mut renamed = draft(AutomationSchedule::Once { at_ms: 2_000_000 });
        renamed.title = "Ya corrió".into();
        store.update(&once, renamed, 3_000_000).unwrap();
        // Moved to a moment that has passed, it is refused as before.
        assert!(store.update(&once, draft(AutomationSchedule::Once { at_ms: 2_500_000 }), 3_000_000).is_err());
    }

    fn native_settings(model_id: &str) -> NativeProviderSettings {
        NativeProviderSettings {
            provider_id: "openai".to_owned(),
            model_id: model_id.to_owned(),
            custom_endpoint: "https://api.openai.com/v1".to_owned(),
        }
    }

    fn native_draft(schedule: AutomationSchedule) -> AutomationDraft {
        let mut value = draft(schedule);
        value.backend = "native".into();
        value.native_provider = Some(native_settings("gpt-4.1"));
        value
    }

    fn origin_for(dispatch: &AutomationDispatch) -> NativeAutomationOrigin {
        // Core validates exact metadata bytes and binding. Execution authority
        // is independently parsed by the host DTO; this fixture does not claim
        // acceptance of that schema or authorize a worker.
        NativeAutomationOrigin::new(
            &dispatch.automation_id,
            &dispatch.run_id,
            dispatch.scheduled_at,
            dispatch.manual,
            "{ \"ledger_fixture\": \"original read-only metadata\" }",
            &dispatch.prompt,
            &dispatch.native_provider.as_ref().unwrap().model_id,
        )
        .unwrap()
    }

    #[test]
    fn native_draft_validation_preserves_arbitrary_provider_and_exact_model_without_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("project");
        fs::create_dir(&workspace).unwrap();
        let mut store = AutomationStore::open(directory.path(), &workspace).unwrap();
        let mut value = native_draft(AutomationSchedule::Once { at_ms: 2_000_000 });
        value.native_provider = Some(NativeProviderSettings {
            provider_id: "independent_42-provider".into(),
            model_id: "organization/Exact Ñ-Model v2 🦀".into(),
            // Managed providers may ignore this stored user endpoint. Actual
            // destination validation belongs to registry-aware dispatch.
            custom_endpoint: String::new(),
        });
        value.validate().unwrap();
        let id = store.create(value.clone(), 1_000_000).unwrap();
        assert_eq!(
            store.automations()[0].native_provider,
            value.native_provider
        );
        value.native_provider.as_mut().unwrap().model_id = "Other/Exact-Model-ß".into();
        store.update(&id, value.clone(), 1_000_010).unwrap();
        let path = store.path.clone();
        drop(store);
        let mut store = AutomationStore::open(directory.path(), &workspace).unwrap();
        assert_eq!(
            store.automations()[0].native_provider,
            value.native_provider
        );
        assert!(store.runs().is_empty());
        let before = fs::read(&path).unwrap();
        let items = store.automations().to_vec();
        let mut invalid = Vec::new();
        let mut missing = value.clone();
        missing.native_provider = None;
        invalid.push(missing);
        for provider in ["", "UPPER", "has space", "has\ncontrol", &"p".repeat(65)] {
            let mut candidate = value.clone();
            candidate.native_provider.as_mut().unwrap().provider_id = provider.into();
            invalid.push(candidate);
        }
        for model in [
            "",
            "  ",
            "model\u{0000}",
            &"m".repeat(openide_core::MAX_MODEL_ROUTE_MODEL_BYTES + 1),
        ] {
            let mut candidate = value.clone();
            candidate.native_provider.as_mut().unwrap().model_id = model.into();
            invalid.push(candidate);
        }
        for endpoint in [
            "https://example.invalid\n",
            &"e".repeat(openide_core::MAX_MODEL_ROUTE_ENDPOINT_BYTES + 1),
        ] {
            let mut candidate = value.clone();
            candidate.native_provider.as_mut().unwrap().custom_endpoint = endpoint.into();
            invalid.push(candidate);
        }
        for candidate in invalid {
            assert!(candidate.validate().is_err());
            assert!(store.create(candidate.clone(), 1_000_020).is_err());
            assert!(store.update(&id, candidate, 1_000_020).is_err());
            assert_eq!(store.automations(), items);
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        let mut invalid_stored: serde_json::Value = serde_json::from_slice(&before).unwrap();
        invalid_stored["automations"][0]["native_provider"]["model_id"] = "model\u{0000}".into();
        let invalid_bytes = serde_json::to_vec(&invalid_stored).unwrap();
        fs::write(&path, &invalid_bytes).unwrap();
        assert!(AutomationStore::open(directory.path(), &workspace).is_err());
        assert_eq!(fs::read(&path).unwrap(), invalid_bytes);
        fs::write(path, before).unwrap();
    }

    /// A real frozen read-only origin and an admitted journal, without model
    /// transport or account credentials. The result factory replays these
    /// persisted events; the scheduler is never used as execution authority.
    pub(crate) fn native_result_fixture(
        root: &Path,
        workspace: &Path,
        manual: bool,
    ) -> (
        AutomationStore,
        SessionStore,
        CheckpointFileStore,
        AutomationDispatch,
        Session,
    ) {
        fs::create_dir_all(workspace).unwrap();
        let settings = NativeProviderSettings {
            provider_id: "custom".into(),
            model_id: "ledger-exact-model-ñ".into(),
            custom_endpoint: "http://127.0.0.1:9/v1".into(),
        };
        let mut store = AutomationStore::open(root, workspace).unwrap();
        let id = store
            .create(
                AutomationDraft {
                    title: "Resultado nativo".into(),
                    prompt: "Inspect the original project without external effects.".into(),
                    backend: "native".into(),
                    native_provider: Some(settings.clone()),
                    schedule: AutomationSchedule::Once { at_ms: 2_000_000 },
                },
                1_000_000,
            )
            .unwrap();
        let dispatch = if manual {
            store.claim_manual(&id, 1_500_000).unwrap()
        } else {
            store.claim_due(2_000_000).unwrap().remove(0)
        };
        let frozen = FrozenNativeAutomationAuthorityV1::freeze(
            settings.resolve_at(root).unwrap(),
            "custom",
            root,
            workspace,
            None,
            &[],
            None,
            None,
            "Original project instructions, ñ and 🦀.\n".into(),
            dispatch.prompt.clone(),
            &dispatch.run_id,
        )
        .unwrap();
        let sessions = SessionStore::open(root.join("sessions")).unwrap();
        let session = sessions
            .create_native_automation(
                workspace,
                frozen
                    .to_origin(
                        &dispatch.automation_id,
                        &dispatch.run_id,
                        dispatch.scheduled_at,
                        dispatch.manual,
                    )
                    .unwrap(),
            )
            .unwrap();
        assert!(
            NativeAutomationResultV1::derive(
                &sessions.load(&session.id).unwrap().unwrap(),
                &CheckpointFileStore::new(root).unwrap(),
            )
            .is_err(),
            "metadata without admission has no native result"
        );
        sessions
            .admit_native_turn(
                &session.id,
                frozen.first_turn_id(),
                frozen.prompt(),
                frozen.model_id(),
                TurnMode::Build,
            )
            .unwrap();
        let checkpoints = CheckpointFileStore::new(root).unwrap();
        (store, sessions, checkpoints, dispatch, session)
    }

    pub(crate) fn observed_result(
        sessions: &SessionStore,
        checkpoints: &CheckpointFileStore,
        session_id: &str,
    ) -> (StoredSession, ValidatedNativeAutomationResult) {
        let _lease = sessions.try_acquire_native_lease(session_id).unwrap();
        let record = sessions.load(session_id).unwrap().unwrap();
        let result = NativeAutomationResultV1::derive(&record, checkpoints).unwrap();
        (record, result)
    }

    #[test]
    fn native_result_recovers_link_and_extends_exact_journal_without_overwriting_dispatch_failure()
    {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let (mut store, sessions, checkpoints, dispatch, session) =
            native_result_fixture(root.path(), &workspace, true);
        let (record, first) = observed_result(&sessions, &checkpoints, &session.id);
        assert_eq!(first.last_sequence(), 1);
        assert!(first.recovery_required());
        assert!(store
            .record_native_result(&record, &first, 1_500_010)
            .unwrap());
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Pending);
        assert_eq!(
            store.runs()[0].session_id.as_deref(),
            Some(session.id.as_str())
        );
        store
            .complete_run(
                &dispatch.run_id,
                Err("Worker admission cut".into()),
                1_500_020,
            )
            .unwrap();
        drop(store);
        let mut reopened = AutomationStore::open(root.path(), &workspace).unwrap();
        let bytes = fs::read(&reopened.path).unwrap();
        assert!(!reopened
            .record_native_result(&record, &first, 1_500_030)
            .unwrap());
        assert_eq!(fs::read(&reopened.path).unwrap(), bytes);
        assert_eq!(reopened.runs()[0].status, AutomationRunStatus::Failed);
        assert_eq!(
            reopened.runs()[0].error.as_deref(),
            Some("Worker admission cut")
        );
        sessions
            .append_native_event(&AgentEvent::new(
                &session.id,
                2,
                AgentEventKind::TurnStarted {
                    prompt: dispatch.prompt.clone(),
                },
            ))
            .unwrap();
        let (record, second) = observed_result(&sessions, &checkpoints, &session.id);
        assert!(reopened
            .record_native_result(&record, &second, 1_500_040)
            .unwrap());
        assert_eq!(second.last_sequence(), 2);
        sessions
            .append_native_event(&AgentEvent::new(
                &session.id,
                3,
                AgentEventKind::TurnFailed {
                    error: "Explicit failed turn before inference".into(),
                },
            ))
            .unwrap();
        let (record, terminal) = observed_result(&sessions, &checkpoints, &session.id);
        assert_eq!(
            terminal.outcome(),
            crate::native_automation_result::NativeAutomationOutcome::Failed
        );
        assert_eq!(terminal.terminal_sequence(), Some(3));
        assert!(!terminal.recovery_required());
        assert!(reopened
            .record_native_result(&record, &terminal, 1_500_050)
            .unwrap());
        assert_eq!(reopened.runs()[0].status, AutomationRunStatus::Failed);
        assert_eq!(
            reopened.runs()[0].error.as_deref(),
            Some("Worker admission cut")
        );
        drop(reopened);
        let reopened = AutomationStore::open(root.path(), &workspace).unwrap();
        assert_eq!(
            reopened.runs()[0].native_result.as_ref(),
            Some(terminal.snapshot())
        );
        assert_eq!(sessions.load(&session.id).unwrap().unwrap(), record);
    }

    #[test]
    fn native_result_rejects_stale_changed_prefix_and_terminal_conflicts_without_writing() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let (mut store, sessions, checkpoints, dispatch, session) =
            native_result_fixture(root.path(), &workspace, false);
        let (first_record, first) = observed_result(&sessions, &checkpoints, &session.id);
        store
            .record_native_result(&first_record, &first, 2_000_010)
            .unwrap();
        let before = fs::read(&store.path).unwrap();
        let runs = store.runs().to_vec();
        let mut changed_same_sequence = first_record.clone();
        changed_same_sequence.events[0].timestamp_ms += 1;
        let changed =
            NativeAutomationResultV1::derive(&changed_same_sequence, &checkpoints).unwrap();
        assert!(matches!(
            store.record_native_result(&changed_same_sequence, &changed, 2_000_020),
            Err(AutomationError::Conflict(_))
        ));
        changed_same_sequence.events.push(AgentEvent::new(
            &session.id,
            2,
            AgentEventKind::TurnStarted {
                prompt: dispatch.prompt.clone(),
            },
        ));
        let changed =
            NativeAutomationResultV1::derive(&changed_same_sequence, &checkpoints).unwrap();
        assert!(matches!(
            store.record_native_result(&changed_same_sequence, &changed, 2_000_030),
            Err(AutomationError::Conflict(_))
        ));
        assert_eq!(store.runs(), runs);
        assert_eq!(fs::read(&store.path).unwrap(), before);
        sessions
            .append_native_event(&AgentEvent::new(
                &session.id,
                2,
                AgentEventKind::TurnStarted {
                    prompt: dispatch.prompt,
                },
            ))
            .unwrap();
        let (record, newer) = observed_result(&sessions, &checkpoints, &session.id);
        store
            .record_native_result(&record, &newer, 2_000_040)
            .unwrap();
        let before = fs::read(&store.path).unwrap();
        assert!(matches!(
            store.record_native_result(&first_record, &first, 2_000_050),
            Err(AutomationError::Conflict(_))
        ));
        assert!(store
            .record_native_result(&first_record, &newer, 2_000_050)
            .is_err());
        assert_eq!(fs::read(&store.path).unwrap(), before);
        sessions
            .append_native_event(&AgentEvent::new(
                &session.id,
                3,
                AgentEventKind::TurnCancelled,
            ))
            .unwrap();
        let (record, terminal) = observed_result(&sessions, &checkpoints, &session.id);
        store
            .record_native_result(&record, &terminal, 2_000_060)
            .unwrap();
        let mut changed_terminal = record.clone();
        changed_terminal.events[2].kind = AgentEventKind::TurnFailed {
            error: "Changed terminal".into(),
        };
        let changed = NativeAutomationResultV1::derive(&changed_terminal, &checkpoints).unwrap();
        let before = fs::read(&store.path).unwrap();
        assert!(matches!(
            store.record_native_result(&changed_terminal, &changed, 2_000_070),
            Err(AutomationError::Conflict(_))
        ));
        assert_eq!(fs::read(&store.path).unwrap(), before);
    }

    #[test]
    fn native_result_rejects_scope_links_skipped_and_trimmed_claims_and_rolls_back_save_failure() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let (mut store, sessions, checkpoints, dispatch, session) =
            native_result_fixture(root.path(), &workspace, false);
        let (record, result) = observed_result(&sessions, &checkpoints, &session.id);
        let original = store.document.clone();
        let foreign_workspace = root.path().join("foreign-project");
        fs::create_dir(&foreign_workspace).unwrap();
        let mut foreign_store = AutomationStore::open(root.path(), &foreign_workspace).unwrap();
        let mut foreign_document = original.clone();
        foreign_document.workspace_key = workspace_key(&foreign_workspace.canonicalize().unwrap());
        foreign_store.commit(foreign_document).unwrap();
        let foreign_before = fs::read(&foreign_store.path).unwrap();
        assert!(matches!(
            foreign_store.record_native_result(&record, &result, 2_000_015),
            Err(AutomationError::Conflict(_))
        ));
        assert_eq!(fs::read(&foreign_store.path).unwrap(), foreign_before);
        assert_eq!(store.runs(), original.runs);
        assert_eq!(store.automations(), original.automations);
        for change in 0..5 {
            let mut next = original.clone();
            match change {
                0 => next.runs[0].status = AutomationRunStatus::Skipped,
                1 => next.runs[0].session_id = Some("different-session".into()),
                2 => next.runs[0].manual = true,
                3 => next.runs[0].scheduled_at += 1,
                _ => next.runs[0].automation_id = Uuid::new_v4().to_string(),
            }
            store.commit(next).unwrap();
            let bytes = fs::read(&store.path).unwrap();
            let runs = store.runs().to_vec();
            assert!(matches!(
                store.record_native_result(&record, &result, 2_000_020),
                Err(AutomationError::Conflict(_))
            ));
            assert_eq!(store.runs(), runs);
            assert_eq!(fs::read(&store.path).unwrap(), bytes);
        }
        store.commit(original).unwrap();
        let path = store.path.clone();
        let bytes = fs::read(&path).unwrap();
        let runs = store.runs().to_vec();
        let blocker = root.path().join("regular-file");
        fs::write(&blocker, b"no directory").unwrap();
        store.path = blocker.join("ledger.json");
        assert!(matches!(
            store.record_native_result(&record, &result, 2_000_030),
            Err(AutomationError::Io(_))
        ));
        assert_eq!(store.runs(), runs);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        store.path = path;
        // Failure of the dispatch before attachment must still permit exact
        // provenance recovery; its error and status remain dispatch metadata.
        store
            .complete_run(&dispatch.run_id, Err("No worker started".into()), 2_000_040)
            .unwrap();
        assert!(store
            .record_native_result(&record, &result, 2_000_050)
            .unwrap());
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Failed);
        let id = dispatch.automation_id;
        for index in 0..MAX_RUNS_IN_LEDGER {
            store.claim_manual(&id, 3_000_000 + index as i64).unwrap();
        }
        assert!(!store.runs().iter().any(|run| run.run_id == dispatch.run_id));
        let bytes = fs::read(&store.path).unwrap();
        assert!(matches!(
            store.record_native_result(&record, &result, 4_000_000),
            Err(AutomationError::NotFound)
        ));
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
        assert_eq!(sessions.load(&session.id).unwrap().unwrap(), record);
    }

    #[test]
    fn native_result_survives_pending_reopen_and_invalid_persisted_bindings_do_not_get_rewritten() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let (mut store, sessions, checkpoints, _, session) =
            native_result_fixture(root.path(), &workspace, false);
        let (record, result) = observed_result(&sessions, &checkpoints, &session.id);
        store
            .record_native_result(&record, &result, 2_000_010)
            .unwrap();
        let path = store.path.clone();
        drop(store);
        let mut reopened = AutomationStore::open(root.path(), &workspace).unwrap();
        assert_eq!(reopened.runs()[0].status, AutomationRunStatus::Interrupted);
        assert_eq!(
            reopened.runs()[0].native_result.as_ref(),
            Some(result.snapshot())
        );
        let bytes = fs::read(&path).unwrap();
        assert!(!reopened
            .record_native_result(&record, &result, 2_000_020)
            .unwrap());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        drop(reopened);
        for field in [
            "session_id",
            "manual",
            "scheduled_at",
            "status",
            "result_unknown_field",
        ] {
            let mut changed = original.clone();
            match field {
                "session_id" => changed["runs"][0][field] = "different-session".into(),
                "manual" => changed["runs"][0][field] = true.into(),
                "scheduled_at" => changed["runs"][0][field] = 2_000_001.into(),
                "status" => changed["runs"][0][field] = "skipped".into(),
                _ => changed["runs"][0]["native_result"][field] = "unsupported".into(),
            }
            let changed_bytes = serde_json::to_vec_pretty(&changed).unwrap();
            fs::write(&path, &changed_bytes).unwrap();
            assert!(
                AutomationStore::open(root.path(), &workspace).is_err(),
                "accepted {field}"
            );
            assert_eq!(fs::read(&path).unwrap(), changed_bytes);
            assert_eq!(sessions.load(&session.id).unwrap().unwrap(), record);
        }
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            AutomationStore::open(root.path(), &workspace)
                .unwrap()
                .runs()[0]
                .native_result
                .as_ref(),
            Some(result.snapshot())
        );
    }

    #[test]
    fn native_attachment_recovers_interrupted_claim_without_dispatch_or_replacing_frozen_origin() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        fs::create_dir(&workspace).unwrap();
        let sessions = SessionStore::open(root.path().join("sessions")).unwrap();
        let mut store = AutomationStore::open(root.path(), &workspace).unwrap();
        let id = store
            .create(
                native_draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        let dispatch = store.claim_due(2_000_000).unwrap().remove(0);
        assert!(!dispatch.manual);
        assert_eq!(store.runs()[0].manual, dispatch.manual);
        let frozen = origin_for(&dispatch);
        let session = sessions
            .create_native_automation(&workspace, frozen.clone())
            .unwrap();
        let mut edited = native_draft(AutomationSchedule::Once { at_ms: 3_000_000 });
        edited.prompt = "Una instrucción posterior no reemplaza el origen".into();
        edited.native_provider = Some(native_settings("gpt-6-sol"));
        store.update(&id, edited, 2_000_010).unwrap();
        let path = store.path.clone();
        drop(store);
        let mut reopened = AutomationStore::open(root.path(), &workspace).unwrap();
        assert_eq!(reopened.runs()[0].status, AutomationRunStatus::Interrupted);
        assert!(reopened
            .attach_existing_native_session(&dispatch.run_id, &session, 2_000_020)
            .unwrap());
        assert_eq!(reopened.runs()[0].status, AutomationRunStatus::Interrupted);
        assert_eq!(
            reopened.runs()[0].session_id.as_deref(),
            Some(session.id.as_str())
        );
        let saved = fs::read(&path).unwrap();
        assert!(!reopened
            .attach_existing_native_session(&dispatch.run_id, &session, 2_000_030)
            .unwrap());
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert_eq!(
            sessions
                .load(&session.id)
                .unwrap()
                .unwrap()
                .session
                .native_automation,
            Some(frozen)
        );
        assert!(sessions
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
        assert!(reopened.claim_due(2_000_030).unwrap().is_empty());
        assert_eq!(reopened.runs().len(), 1);
        drop(reopened);
        let reopened = AutomationStore::open(root.path(), &workspace).unwrap();
        assert_eq!(reopened.runs()[0].status, AutomationRunStatus::Interrupted);
        assert_eq!(
            reopened.runs()[0].session_id.as_deref(),
            Some(session.id.as_str())
        );
    }

    #[test]
    fn native_attachment_rejects_foreign_or_invalid_origins_without_writing_ledger() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let foreign_workspace = root.path().join("other-project");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&foreign_workspace).unwrap();
        let sessions = SessionStore::open(root.path().join("sessions")).unwrap();
        let mut store = AutomationStore::open(root.path(), &workspace).unwrap();
        store
            .create(
                native_draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        let dispatch = store.claim_due(2_000_000).unwrap().remove(0);
        let origin = origin_for(&dispatch);
        let session = sessions
            .create_native_automation(&workspace, origin.clone())
            .unwrap();
        let foreign = sessions
            .create_native_automation(&foreign_workspace, origin.clone())
            .unwrap();
        let other_id = Uuid::new_v4().to_string();
        let other_origin = NativeAutomationOrigin::new(
            &other_id,
            format!("{other_id}:2000000"),
            2_000_000,
            false,
            "{}",
            &dispatch.prompt,
            "gpt-4.1",
        )
        .unwrap();
        let other_automation = sessions
            .create_native_automation(&workspace, other_origin)
            .unwrap();
        let next_origin = NativeAutomationOrigin::new(
            &dispatch.automation_id,
            format!("{}:2000001", dispatch.automation_id),
            2_000_001,
            false,
            "{}",
            &dispatch.prompt,
            "gpt-4.1",
        )
        .unwrap();
        let other_run = sessions
            .create_native_automation(&workspace, next_origin)
            .unwrap();
        let mut missing_origin = session.clone();
        missing_origin.native_automation = None;
        let mut invalid_id = session.clone();
        invalid_id.id = Uuid::new_v4().to_string();
        let mut invalid_backend = session.clone();
        invalid_backend.backend = "codex".into();
        let mut invalid_json = session.clone();
        invalid_json
            .native_automation
            .as_mut()
            .unwrap()
            .authority_snapshot_json
            .push(' ');
        let mut invalid_version = session.clone();
        invalid_version
            .native_automation
            .as_mut()
            .unwrap()
            .authority_snapshot_version += 1;
        for candidate in [
            foreign,
            other_automation,
            other_run,
            missing_origin,
            invalid_id,
            invalid_backend,
            invalid_json,
            invalid_version,
        ] {
            let bytes = fs::read(&store.path).unwrap();
            let runs = store.runs().to_vec();
            assert!(store
                .attach_existing_native_session(&dispatch.run_id, &candidate, 2_000_020)
                .is_err());
            assert_eq!(store.runs(), runs);
            assert_eq!(fs::read(&store.path).unwrap(), bytes);
        }
        let mut inconsistent_manual = store.document.clone();
        inconsistent_manual.runs[0].manual = true;
        store.commit(inconsistent_manual).unwrap();
        let bytes = fs::read(&store.path).unwrap();
        assert!(matches!(
            store.attach_existing_native_session(&dispatch.run_id, &session, 2_000_030),
            Err(AutomationError::Conflict(_))
        ));
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
        let mut conflicting_link = store.document.clone();
        conflicting_link.runs[0].manual = false;
        conflicting_link.runs[0].session_id = Some("another-already-linked-session".into());
        store.commit(conflicting_link).unwrap();
        let bytes = fs::read(&store.path).unwrap();
        assert!(matches!(
            store.attach_existing_native_session(&dispatch.run_id, &session, 2_000_040),
            Err(AutomationError::Conflict(_))
        ));
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
    }

    #[test]
    fn native_attachment_keeps_link_after_dispatch_failure_and_preserves_claim_on_save_error() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        fs::create_dir(&workspace).unwrap();
        let sessions = SessionStore::open(root.path().join("sessions")).unwrap();
        let mut store = AutomationStore::open(root.path(), &workspace).unwrap();
        let id = store
            .create(
                native_draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        let dispatch = store.claim_manual(&id, 1_500_000).unwrap();
        assert!(dispatch.manual);
        assert_eq!(store.runs()[0].manual, dispatch.manual);
        let session = sessions
            .create_native_automation(&workspace, origin_for(&dispatch))
            .unwrap();
        let original = store.document.clone();
        let mut wrong_schedule = original.clone();
        wrong_schedule.runs[0].scheduled_at += 1;
        store.commit(wrong_schedule).unwrap();
        let bytes = fs::read(&store.path).unwrap();
        assert!(matches!(
            store.attach_existing_native_session(&dispatch.run_id, &session, 1_500_005),
            Err(AutomationError::Conflict(_))
        ));
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
        store.commit(original).unwrap();
        let path = store.path.clone();
        let bytes = fs::read(&path).unwrap();
        let runs = store.runs().to_vec();
        let blocked_parent = root.path().join("not-a-directory");
        fs::write(
            &blocked_parent,
            b"regular file prevents creating staging directory",
        )
        .unwrap();
        store.path = blocked_parent.join("ledger.json");
        assert!(matches!(
            store.attach_existing_native_session(&dispatch.run_id, &session, 1_500_010),
            Err(AutomationError::Io(_))
        ));
        assert_eq!(store.runs(), runs);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        store.path = path;
        assert!(store
            .attach_existing_native_session(&dispatch.run_id, &session, 1_500_020)
            .unwrap());
        let bytes = fs::read(&store.path).unwrap();
        assert!(matches!(
            store.complete_run(
                &dispatch.run_id,
                Ok("replacement-session".into()),
                1_500_030
            ),
            Err(AutomationError::Conflict(_))
        ));
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
        store
            .complete_run(
                &dispatch.run_id,
                Err("El worker no pudo arrancar".into()),
                1_500_040,
            )
            .unwrap();
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Failed);
        assert_eq!(
            store.runs()[0].session_id.as_deref(),
            Some(session.id.as_str())
        );
        let bytes = fs::read(&store.path).unwrap();
        store
            .complete_run(
                &dispatch.run_id,
                Err("El worker no pudo arrancar".into()),
                1_500_050,
            )
            .unwrap();
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
        drop(store);
        let store = AutomationStore::open(root.path(), &workspace).unwrap();
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Failed);
        assert_eq!(
            store.runs()[0].session_id.as_deref(),
            Some(session.id.as_str())
        );
        assert!(sessions
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
    }

    #[test]
    fn native_attachment_is_idempotent_for_pending_and_confirmed_dispatch_without_claiming_success()
    {
        let root = tempfile::tempdir().unwrap();
        let sessions = SessionStore::open(root.path().join("sessions")).unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let id = store
            .create(
                native_draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        let dispatch = store.claim_manual(&id, 1_100_000).unwrap();
        let session = sessions
            .create_native_automation(root.path(), origin_for(&dispatch))
            .unwrap();
        assert!(store
            .attach_existing_native_session(&dispatch.run_id, &session, 1_100_010)
            .unwrap());
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Pending);
        let bytes = fs::read(&store.path).unwrap();
        assert!(!store
            .attach_existing_native_session(&dispatch.run_id, &session, 1_100_020)
            .unwrap());
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
        store
            .complete_run(&dispatch.run_id, Ok(session.id.clone()), 1_100_030)
            .unwrap();
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Dispatched);
        let path = store.path.clone();
        drop(store);
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(!store
            .attach_existing_native_session(&dispatch.run_id, &session, 1_100_040)
            .unwrap());
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(store.runs()[0].status, AutomationRunStatus::Dispatched);
        assert!(sessions
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
    }

    #[test]
    fn trimmed_native_claim_is_not_recreated_from_retained_session_metadata() {
        let root = tempfile::tempdir().unwrap();
        let sessions = SessionStore::open(root.path().join("sessions")).unwrap();
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let id = store
            .create(
                native_draft(AutomationSchedule::Once { at_ms: 2_000_000 }),
                1_000_000,
            )
            .unwrap();
        let dispatch = store.claim_manual(&id, 1_100_000).unwrap();
        let session = sessions
            .create_native_automation(root.path(), origin_for(&dispatch))
            .unwrap();
        for offset in 0..MAX_RUNS_IN_LEDGER {
            store.claim_manual(&id, 1_100_001 + offset as i64).unwrap();
        }
        assert_eq!(store.runs().len(), MAX_RUNS_IN_LEDGER);
        assert!(!store.runs().iter().any(|run| run.run_id == dispatch.run_id));
        let path = store.path.clone();
        drop(store);
        let mut store = AutomationStore::open(root.path(), root.path()).unwrap();
        let bytes = fs::read(&path).unwrap();
        let runs = store.runs().to_vec();
        assert!(matches!(
            store.attach_existing_native_session(&dispatch.run_id, &session, 1_200_000),
            Err(AutomationError::NotFound)
        ));
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(store.runs(), runs);
        assert!(sessions
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
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
        // Its run goes with it.
        assert!(store.runs().is_empty());
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
