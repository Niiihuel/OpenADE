//! Per-file review receipts. Accepting records a decision about immutable
//! evidence; it never changes a workspace file, the journal, a baseline or Git.
use super::*;
use crate::session_changes_editor::{lock_workspace, require_private_file, Directory};

#[path = "session_file_review_decisions.rs"]
mod decisions;
pub(crate) use decisions::{
    decide_file_review, FileReviewBlock, FileReviewDecision, FileReviewDecisionTarget,
};

const DIRECTORY: &str = "session-file-reviews";
const MAX_RECEIPT_BYTES: usize = 16 * 1024;
const MAX_RECEIPTS: usize = 4096;

#[derive(Clone, Debug)]
pub(crate) struct FileReview {
    pub session_id: String,
    pub workspace: PathBuf,
    pub tool_call_id: String,
    pub path: String,
    pub old_path: Option<String>,
    pub sequence: u64,
    pub before: Option<Arc<str>>,
    pub after: Option<Arc<str>>,
    pub file: ReviewFile,
    original_file: ReviewFile,
    pub version: String,
    /// Version of durable review decisions, independent of immutable evidence.
    pub decision_version: String,
    /// Expected disk content after any rejected blocks; `after` stays immutable.
    pub current: Option<Arc<str>>,
    pub blocks: Arc<[FileReviewBlock]>,
    pub resolved: bool,
    pub can_reject: bool,
    pub rejection_unavailable: Option<String>,
    pub accepted: bool,
    pub can_accept: bool,
    pub acceptance_unavailable: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileReviewAcceptance {
    Accepted,
    AlreadyAccepted,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u8,
    session_id: String,
    workspace: PathBuf,
    tool_call_id: String,
    path: String,
    old_path: Option<String>,
    sequence: u64,
    version: String,
    accepted_at_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReceipt {
    sha256: String,
    payload: Receipt,
}

/// Runs on a background worker. Full original and modified text are returned
/// only when both sides are provable; a hunk preview is never a whole document.
pub(crate) fn load_file_review(
    storage: &Path,
    session: &str,
    workspace: &Path,
    call_id: &str,
    path: &str,
) -> io::Result<FileReview> {
    let _guard = lock_workspace(storage, workspace)?;
    load_guarded(storage, session, workspace, call_id, path)
}

/// Find the latest admission touching this path, not merely the latest
/// successful result. A newer failed/pending mutation must not reveal an
/// older revision as if it were the current pending review.
pub(crate) fn load_latest_file_review(
    storage: &Path,
    session: &str,
    workspace: &Path,
    path: &str,
) -> io::Result<Option<FileReview>> {
    let _guard = lock_workspace(storage, workspace)?;
    let root = workspace.canonicalize()?;
    baseline_path(storage, session)?;
    let path = normalized_path(path).map_err(|error| invalid(&error))?;
    let events = match read_stored_journal(storage, session)? {
        StoredJournal::Events(events) => events,
        StoredJournal::Unavailable(reason) => return Err(invalid(&reason)),
    };
    validate_journal(session, &root, &events)?;
    let recorded = events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::SessionStarted { workspace } => Some(workspace.as_path()),
            _ => None,
        })
        .unwrap_or(&root);
    let roots = [&root as &Path, recorded];
    let mut latest = None;
    for event in events.iter().rev() {
        let AgentEventKind::ToolStarted { tool_call } = &event.kind else {
            continue;
        };
        if !matches!(tool_call.name.as_str(), "write_file" | "apply_patch") {
            continue;
        }
        // An unparseable newer mutation cannot establish that this path was
        // untouched. Do not search behind it for an older actionable review.
        let paths = call_paths(tool_call, &roots).map_err(|error| invalid(&error))?;
        if paths.contains(&path) {
            latest = Some(tool_call.id.clone());
            break;
        }
    }
    let Some(call_id) = latest else {
        return Ok(None);
    };
    load_guarded_with_events(storage, session, &root, &call_id, &path, Some(events)).map(Some)
}

#[cfg(test)]
fn accept_file_review(
    storage: &Path,
    session: &str,
    workspace: &Path,
    call_id: &str,
    path: &str,
    expected_version: &str,
) -> io::Result<FileReviewAcceptance> {
    let _guard = lock_workspace(storage, workspace)?;
    let review = load_guarded(storage, session, workspace, call_id, path)?;
    if review.version != expected_version {
        return Err(invalid(
            "La revisión cambió. Abre de nuevo los cambios del archivo.",
        ));
    }
    // An existing receipt acknowledges only this exact historical version.
    // Repeating it cannot accept an intervening edit with a different token.
    if review.accepted {
        return Ok(FileReviewAcceptance::AlreadyAccepted);
    }
    if !review.can_accept {
        return Err(invalid(
            review
                .acceptance_unavailable
                .as_deref()
                .unwrap_or("Esta versión no se puede aceptar."),
        ));
    }
    let directory = Directory::open_named(storage, DIRECTORY, true)?
        .ok_or_else(|| invalid("No se pudo abrir el registro de revisiones"))?;
    if fs::read_dir(storage.join(DIRECTORY))?
        .take(MAX_RECEIPTS)
        .count()
        >= MAX_RECEIPTS
    {
        return Err(invalid("El registro de revisiones alcanzó su límite"));
    }
    let payload = Receipt {
        schema_version: 1,
        session_id: review.session_id.clone(),
        workspace: review.workspace.clone(),
        tool_call_id: review.tool_call_id.clone(),
        path: review.path.clone(),
        old_path: review.old_path.clone(),
        sequence: review.sequence,
        version: review.version.clone(),
        accepted_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64,
    };
    let sha256 = digest(&serde_json::to_vec(&payload).map_err(io::Error::other)?);
    let encoded =
        serde_json::to_vec(&StoredReceipt { sha256, payload }).map_err(io::Error::other)?;
    if encoded.len() > MAX_RECEIPT_BYTES {
        return Err(invalid("La revisión supera su límite de metadatos"));
    }
    let temporary = format!(".{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file = directory.create(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        revalidate_acceptance(storage, &review)?;
        directory.publish(&temporary, &receipt_name(&review))?;
        directory.sync()
    })();
    let _ = directory.remove(&temporary);
    result?;
    Ok(FileReviewAcceptance::Accepted)
}

fn load_guarded(
    storage: &Path,
    session: &str,
    workspace: &Path,
    call_id: &str,
    requested_path: &str,
) -> io::Result<FileReview> {
    load_guarded_with_events(storage, session, workspace, call_id, requested_path, None)
}

fn load_guarded_with_events(
    storage: &Path,
    session: &str,
    workspace: &Path,
    call_id: &str,
    requested_path: &str,
    supplied_events: Option<Vec<AgentEvent>>,
) -> io::Result<FileReview> {
    let started = Instant::now();
    let root = workspace.canonicalize()?;
    baseline_path(storage, session)?;
    let path = normalized_path(requested_path).map_err(|error| invalid(&error))?;
    if call_id.is_empty() || call_id.len() > 1024 || call_id.chars().any(char::is_control) {
        return Err(invalid("Identificador de edición inválido"));
    }
    let events = match supplied_events {
        Some(events) => events,
        None => match read_stored_journal(storage, session)? {
            StoredJournal::Events(events) => events,
            StoredJournal::Unavailable(reason) => return Err(invalid(&reason)),
        },
    };
    validate_journal(session, &root, &events)?;
    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::ToolStarted { tool_call } if tool_call.id == call_id => {
                Some((event, tool_call))
            }
            _ => None,
        })
        .collect();
    let finishes: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(&event.kind,
        AgentEventKind::ToolFinished { tool_call_id, .. } if tool_call_id == call_id)
        })
        .collect();
    if starts.len() != 1 || finishes.len() != 1 {
        return Err(invalid(
            "La edición no tiene una entrada y un resultado únicos",
        ));
    }
    let (start, call) = starts[0];
    let finish = finishes[0];
    if finish.sequence <= start.sequence
        || !matches!(
            &finish.kind,
            AgentEventKind::ToolFinished {
                is_error: false,
                ..
            }
        )
    {
        return Err(invalid(
            "La edición no tiene un resultado exitoso confirmado",
        ));
    }
    let recorded = events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::SessionStarted { workspace } => Some(workspace.as_path()),
            _ => None,
        })
        .ok_or_else(|| invalid("Falta la carpeta registrada de esta conversación"))?;
    let roots = [&root as &Path, recorded];
    if !call_paths(call, &roots)
        .map_err(|error| invalid(&error))?
        .contains(&path)
    {
        return Err(invalid("El archivo no pertenece a esta edición"));
    }
    let transition = match tool_snapshots::load_transition(
        storage,
        session,
        &root,
        recorded,
        call,
        start,
        finish.sequence,
    )? {
        Some(exact) => exact,
        None => {
            let baseline = read_baseline(storage, session, &root)?.ok_or_else(|| {
                invalid("No hay una versión anterior verificable de esta edición")
            })?;
            replay_session(
                storage,
                session,
                &root,
                &baseline,
                &events,
                started,
                Some(call_id),
            )?
            .selected
            .ok_or_else(|| invalid("No se pudo reconstruir esta edición"))?
        }
    };
    if let Some(reason) = transition.unavailable {
        return Err(invalid(&reason));
    }
    if transition.renames.values().any(|source| source == &path) {
        return Err(invalid(
            "Abre la ruta de destino para revisar este renombrado",
        ));
    }
    let old_path = transition.renames.get(&path).cloned();
    if let Some(source) = &old_path {
        if !matches!(transition.after.get(source), Some(Ok(None))) {
            return Err(invalid("No se puede verificar el origen del renombrado"));
        }
    }
    let bytes = |map: &BTreeMap<String, Result<Option<Vec<u8>>, String>>, key: &str| {
        map.get(key)
            .ok_or_else(|| invalid("La edición no conserva esta versión del archivo"))?
            .clone()
            .map_err(|error| invalid(&error))
    };
    let before = bytes(&transition.before, old_path.as_deref().unwrap_or(&path))?;
    let after = bytes(&transition.after, &path)?;
    let text = |value: &Option<Vec<u8>>| -> io::Result<Option<Arc<str>>> {
        value
            .as_ref()
            .map(|bytes| {
                if bytes.len() > MAX_FILE_BYTES || bytes.contains(&0) {
                    return Err(invalid(
                        "La revisión requiere un archivo de texto dentro del límite",
                    ));
                }
                Ok(Arc::<str>::from(
                    std::str::from_utf8(bytes).map_err(io::Error::other)?,
                ))
            })
            .transpose()
    };
    let before_text = text(&before)?;
    let after_text = text(&after)?;
    let (mut file, truncated, _) = snapshot_diff_until(
        storage,
        &path,
        before.as_deref().unwrap_or_default(),
        after.as_deref().unwrap_or_default(),
        MAX_DIFF_BYTES,
        started + PROJECTION_BUDGET,
    )?;
    file.status = if old_path.is_some() {
        "Renombrado"
    } else {
        match (before.is_some(), after.is_some()) {
            (false, true) => "Nuevo",
            (true, false) => "Eliminado",
            _ => "Modificado",
        }
    }
    .into();
    file.old_path = old_path.clone();
    let version = review_version(
        session,
        &root,
        call,
        start,
        finish,
        &path,
        old_path.as_deref(),
        before.as_deref(),
        after.as_deref(),
    )?;
    let paths = std::iter::once(path.clone())
        .chain(old_path.iter().cloned())
        .collect();
    let mut unavailable =
        later_or_pending_mutation(&events, call_id, start.sequence, &paths, &roots);
    if unavailable.is_none() && before == after && old_path.is_none() {
        unavailable = Some("Esta edición no cambió el contenido del archivo.".into());
    }
    if unavailable.is_none() && truncated {
        unavailable =
            Some("El diff está incompleto; no se puede aceptar esta revisión parcial.".into());
    }
    let mut review = FileReview {
        session_id: session.into(),
        workspace: root,
        tool_call_id: call_id.into(),
        path,
        old_path,
        sequence: finish.sequence,
        before: before_text,
        after: after_text.clone(),
        file,
        original_file: ReviewFile::default(),
        version,
        decision_version: String::new(),
        current: after_text.clone(),
        blocks: Arc::from([]),
        resolved: false,
        can_reject: false,
        rejection_unavailable: None,
        accepted: false,
        can_accept: unavailable.is_none(),
        acceptance_unavailable: unavailable,
    };
    review.original_file = review.file.clone();
    review.accepted = read_receipt(storage, &review)?;
    decisions::project(storage, &mut review)?;
    Ok(review)
}

fn review_version(
    session: &str,
    root: &Path,
    call: &ToolCall,
    start: &AgentEvent,
    finish: &AgentEvent,
    path: &str,
    old_path: Option<&str>,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
) -> io::Result<String> {
    Ok(digest(
        &serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "session": session, "workspace": root,
            "call": call, "start": start, "finish": finish,
            "path": path, "old_path": old_path,
            "before": before.map(digest), "after": after.map(digest),
        }))
        .map_err(io::Error::other)?,
    ))
}

/// The initial read can include a bounded diff calculation. Recheck admission
/// history and the observed disk bytes immediately before publishing metadata.
/// This is not a CAS against non-cooperating external writers; receipts always
/// remain scoped to the reviewed version, never a later edit.
fn revalidate_acceptance(storage: &Path, review: &FileReview) -> io::Result<()> {
    let events = match read_stored_journal(storage, &review.session_id)? {
        StoredJournal::Events(events) => events,
        StoredJournal::Unavailable(reason) => return Err(invalid(&reason)),
    };
    validate_journal(&review.session_id, &review.workspace, &events)?;
    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::ToolStarted { tool_call } if tool_call.id == review.tool_call_id => {
                Some((event, tool_call))
            }
            _ => None,
        })
        .collect();
    let finishes: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(&event.kind,
        AgentEventKind::ToolFinished { tool_call_id, .. } if tool_call_id == &review.tool_call_id)
        })
        .collect();
    if starts.len() != 1 || finishes.len() != 1 {
        return Err(invalid(
            "El registro de la edición cambió durante la revisión",
        ));
    }
    let (start, call) = starts[0];
    let version = review_version(
        &review.session_id,
        &review.workspace,
        call,
        start,
        finishes[0],
        &review.path,
        review.old_path.as_deref(),
        review.before.as_deref().map(str::as_bytes),
        review.after.as_deref().map(str::as_bytes),
    )?;
    if version != review.version {
        return Err(invalid("La versión registrada cambió durante la revisión"));
    }
    let recorded = events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::SessionStarted { workspace } => Some(workspace.as_path()),
            _ => None,
        })
        .ok_or_else(|| invalid("Falta la carpeta registrada de esta conversación"))?;
    let paths = std::iter::once(review.path.clone())
        .chain(review.old_path.iter().cloned())
        .collect();
    if let Some(reason) = later_or_pending_mutation(
        &events,
        &review.tool_call_id,
        start.sequence,
        &paths,
        &[&review.workspace, recorded],
    ) {
        return Err(invalid(&reason));
    }
    if let Some(reason) = disk_mismatch(
        &review.workspace,
        &review.path,
        review.current.as_deref().map(str::as_bytes),
    ) {
        return Err(invalid(&reason));
    }
    if let Some(source) = &review.old_path {
        if let Some(reason) = disk_mismatch(&review.workspace, source, None) {
            return Err(invalid(&reason));
        }
    }
    Ok(())
}

fn disk_mismatch(root: &Path, path: &str, expected: Option<&[u8]>) -> Option<String> {
    let actual = match read_workspace_file(root, path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Some("No se puede verificar el archivo actual en disco.".into()),
    };
    (actual.as_deref() != expected).then(|| {
        "El archivo cambió después de esta edición. Revisa su versión más reciente.".into()
    })
}

fn later_or_pending_mutation(
    events: &[AgentEvent],
    selected: &str,
    selected_start: u64,
    paths: &BTreeSet<String>,
    roots: &[&Path],
) -> Option<String> {
    let mut pending = HashMap::<&str, usize>::new();
    let mut hooks = HashMap::<(&str, &str, &str), usize>::new();
    for event in events {
        match &event.kind {
            AgentEventKind::ToolStarted { tool_call } => {
                let edit = matches!(tool_call.name.as_str(), "write_file" | "apply_patch");
                let touches = edit
                    && call_paths(tool_call, roots)
                        .map_or(true, |other| !paths.is_disjoint(&other));
                if tool_call.id != selected && touches && event.sequence > selected_start {
                    return Some(
                        "Hay una edición posterior de este archivo. Revisa la más reciente.".into(),
                    );
                }
                if touches || opaque_effect(&tool_call.name) {
                    *pending.entry(tool_call.id.as_str()).or_default() += 1;
                }
            }
            AgentEventKind::ToolFinished { tool_call_id, .. } => {
                if let Some(count) = pending.get_mut(tool_call_id.as_str()) {
                    *count = count.saturating_sub(1);
                }
            }
            AgentEventKind::HookStarted {
                tool_call_id,
                hook_id,
                phase,
            } => {
                *hooks.entry((tool_call_id, hook_id, phase)).or_default() += 1;
            }
            AgentEventKind::HookFinished {
                tool_call_id,
                hook_id,
                phase,
                ..
            } => {
                if let Some(count) =
                    hooks.get_mut(&(tool_call_id.as_str(), hook_id.as_str(), phase.as_str()))
                {
                    *count = count.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
    (pending
        .values()
        .chain(hooks.values())
        .any(|count| *count > 0))
    .then(|| "Hay una operación pendiente que puede modificar este archivo.".into())
}

fn receipt_name(review: &FileReview) -> String {
    format!("{}.{}.json", review.session_id, review.version)
}

fn read_receipt(storage: &Path, review: &FileReview) -> io::Result<bool> {
    let Some(directory) = Directory::open_named(storage, DIRECTORY, false)? else {
        return Ok(false);
    };
    let file = match directory.read(&receipt_name(review)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    require_private_file(&file.metadata()?)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_RECEIPT_BYTES as u64 {
        return Err(invalid("El registro de aceptación no es válido"));
    }
    let mut bytes = Vec::new();
    file.take((MAX_RECEIPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid("El registro de aceptación es demasiado grande"));
    }
    let record: StoredReceipt = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let value = &record.payload;
    if value.schema_version != 1
        || value.session_id != review.session_id
        || value.workspace != review.workspace
        || value.tool_call_id != review.tool_call_id
        || value.path != review.path
        || value.old_path != review.old_path
        || value.sequence != review.sequence
        || value.version != review.version
        || record.sha256 != digest(&serde_json::to_vec(value).map_err(io::Error::other)?)
    {
        return Err(invalid(
            "El registro de aceptación no corresponde a esta versión",
        ));
    }
    Ok(true)
}

pub(super) fn remove_session(storage: &Path, session: &str) -> io::Result<()> {
    baseline_path(storage, session)?;
    decisions::remove_session(storage, session)?;
    let Some(directory) = Directory::open_named(storage, DIRECTORY, false)? else {
        return Ok(());
    };
    let prefix = format!("{session}.");
    for entry in fs::read_dir(storage.join(DIRECTORY))?.take(MAX_RECEIPTS + 1) {
        let name = entry?.file_name();
        if let Some(name) = name
            .to_str()
            .filter(|name| name.starts_with(&prefix) && name.ends_with(".json"))
        {
            directory.remove(name)?;
        }
    }
    directory.sync()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_harness::{LocalTools, ToolContext};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    struct Fixture {
        _temporary: tempfile::TempDir,
        storage: PathBuf,
        workspace: PathBuf,
        events: Vec<AgentEvent>,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let storage = temporary.path().join("store");
            let workspace = temporary.path().join("workspace");
            fs::create_dir_all(&storage).unwrap();
            fs::create_dir_all(&workspace).unwrap();
            let events = vec![AgentEvent::new(
                "session",
                1,
                AgentEventKind::SessionStarted {
                    workspace: workspace.clone(),
                },
            )];
            Self {
                _temporary: temporary,
                storage,
                workspace,
                events,
            }
        }

        fn event(&mut self, kind: AgentEventKind) {
            self.events.push(AgentEvent::new(
                "session",
                self.events.len() as u64 + 1,
                kind,
            ));
            self.persist();
        }

        fn persist(&self) {
            let mut bytes = Vec::new();
            for event in &self.events {
                serde_json::to_writer(&mut bytes, event).unwrap();
                bytes.push(b'\n');
            }
            fs::write(self.storage.join("session.jsonl"), bytes).unwrap();
        }

        fn legacy_write(&mut self, id: &str, content: &str) {
            self.event(AgentEventKind::ToolStarted {
                tool_call: write(id, "edit.txt", content),
            });
            fs::write(self.workspace.join("edit.txt"), content).unwrap();
            self.event(AgentEventKind::ToolFinished {
                tool_call_id: id.into(),
                output: "file written".into(),
                is_error: false,
            });
        }

        async fn execute(&mut self, call: ToolCall) {
            self.event(AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            });
            let result = with_file_snapshots(
                Arc::new(LocalTools::new().with_write_access(true)),
                self.storage.clone(),
                "session".into(),
                self.workspace.clone(),
            )
            .execute(
                &call,
                &ToolContext {
                    workspace: self.workspace.clone(),
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
            assert!(!result.is_error, "{}", result.content);
            self.event(AgentEventKind::ToolFinished {
                tool_call_id: call.id,
                output: result.content,
                is_error: false,
            });
        }

        fn load(&self, call: &str, path: &str) -> FileReview {
            load_file_review(&self.storage, "session", &self.workspace, call, path).unwrap()
        }

        fn accept(&self, review: &FileReview) -> io::Result<FileReviewAcceptance> {
            accept_file_review(
                &self.storage,
                "session",
                &self.workspace,
                &review.tool_call_id,
                &review.path,
                &review.version,
            )
        }
    }

    fn write(id: &str, path: &str, content: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "write_file".into(),
            arguments: json!({"path":path,"content":content}),
        }
    }

    #[test]
    fn legacy_acceptance_is_durable_idempotent_and_preserves_history_disk_and_git() {
        let mut fixture = Fixture::new();
        fs::write(fixture.workspace.join("edit.txt"), "before\n").unwrap();
        capture_baseline(
            &fixture.storage,
            "session",
            &fixture.workspace,
            &fixture.events,
        )
        .unwrap();
        fixture.legacy_write("first", "after\n");
        fs::create_dir(fixture.workspace.join(".git")).unwrap();
        fs::write(fixture.workspace.join(".git/index"), "untouched index").unwrap();
        let journal = fs::read(fixture.storage.join("session.jsonl")).unwrap();
        let baseline = fs::read(baseline_path(&fixture.storage, "session").unwrap()).unwrap();
        let review = fixture.load("first", "edit.txt");
        assert_eq!(
            (review.before.as_deref(), review.after.as_deref()),
            (Some("before\n"), Some("after\n"))
        );
        assert!(review.can_accept && !review.accepted);
        assert_eq!(
            fixture.accept(&review).unwrap(),
            FileReviewAcceptance::Accepted
        );
        assert_eq!(
            fixture.accept(&review).unwrap(),
            FileReviewAcceptance::AlreadyAccepted
        );
        let reopened = fixture.load("first", "edit.txt");
        assert!(reopened.accepted && !reopened.can_accept);
        assert_eq!(
            reopened.file, review.file,
            "accepted history remains reviewable"
        );
        assert_eq!(
            fs::read(fixture.storage.join("session.jsonl")).unwrap(),
            journal
        );
        assert_eq!(
            fs::read(baseline_path(&fixture.storage, "session").unwrap()).unwrap(),
            baseline
        );
        assert_eq!(
            fs::read_to_string(fixture.workspace.join("edit.txt")).unwrap(),
            "after\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.workspace.join(".git/index")).unwrap(),
            "untouched index"
        );
        fixture.legacy_write("second", "next version\n");
        let second = fixture.load("second", "edit.txt");
        assert!(!second.accepted && second.can_accept);
        assert_ne!(second.version, review.version);
        assert!(accept_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "second",
            "edit.txt",
            &review.version
        )
        .is_err());
        assert_eq!(
            fs::read_dir(fixture.storage.join(DIRECTORY))
                .unwrap()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn exact_snapshots_review_new_deleted_and_renamed_files_without_initial_baseline() {
        let mut fixture = Fixture::new();
        fixture.execute(write("empty", "empty.txt", "")).await;
        let empty = fixture.load("empty", "empty.txt");
        assert_eq!(
            (empty.before.as_deref(), empty.after.as_deref()),
            (None, Some(""))
        );
        assert_eq!(empty.file.status, "Nuevo");
        assert!(empty.can_accept);
        fixture.accept(&empty).unwrap();
        fs::write(fixture.workspace.join("old.txt"), "old line\n").unwrap();
        fs::write(fixture.workspace.join("deleted.txt"), "removed\n").unwrap();
        fixture.execute(ToolCall { id: "patch".into(), name: "apply_patch".into(),
            arguments: json!({"patch":"*** Begin Patch\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-old line\n+new line\n*** Delete File: deleted.txt\n*** End Patch\n"}),
        }).await;
        let moved = fixture.load("patch", "moved.txt");
        assert_eq!(moved.old_path.as_deref(), Some("old.txt"));
        assert_eq!(
            (moved.before.as_deref(), moved.after.as_deref()),
            (Some("old line\n"), Some("new line\n"))
        );
        assert!(moved.can_accept);
        assert!(load_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "patch",
            "old.txt"
        )
        .is_err());
        fixture.accept(&moved).unwrap();
        let deleted = fixture.load("patch", "deleted.txt");
        assert_eq!(
            (deleted.before.as_deref(), deleted.after.as_deref()),
            (Some("removed\n"), None)
        );
        assert!(deleted.can_accept);
        fixture.accept(&deleted).unwrap();
        assert!(load_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "patch",
            "unrelated.txt"
        )
        .is_err());
        assert!(load_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "patch",
            "../outside"
        )
        .is_err());
    }

    #[tokio::test]
    async fn stale_disk_later_edits_and_pending_opaque_effects_cannot_be_accepted() {
        let mut fixture = Fixture::new();
        fs::write(fixture.workspace.join("edit.txt"), "before\n").unwrap();
        fixture.execute(write("first", "edit.txt", "after\n")).await;
        let first = fixture.load("first", "edit.txt");
        fs::write(fixture.workspace.join("edit.txt"), "external edit\n").unwrap();
        let changed = fixture.load("first", "edit.txt");
        assert!(!changed.can_accept && changed.acceptance_unavailable.unwrap().contains("cambió"));
        assert!(fixture.accept(&first).is_err());
        assert_eq!(
            fs::read_to_string(fixture.workspace.join("edit.txt")).unwrap(),
            "external edit\n"
        );
        fs::write(fixture.workspace.join("edit.txt"), "after\n").unwrap();
        fixture.event(AgentEventKind::ToolStarted {
            tool_call: ToolCall {
                id: "command".into(),
                name: "run_command".into(),
                arguments: json!({"command":"pending"}),
            },
        });
        assert!(!fixture.load("first", "edit.txt").can_accept);
        assert!(fixture.accept(&first).is_err());
        fixture.event(AgentEventKind::ToolFinished {
            tool_call_id: "command".into(),
            output: "done".into(),
            is_error: false,
        });
        assert!(fixture.load("first", "edit.txt").can_accept);
        fixture.event(AgentEventKind::HookStarted {
            tool_call_id: "command".into(),
            hook_id: "post".into(),
            phase: "post_tool_use".into(),
        });
        assert!(!fixture.load("first", "edit.txt").can_accept);
        fixture.event(AgentEventKind::HookFinished {
            tool_call_id: "command".into(),
            hook_id: "post".into(),
            phase: "post_tool_use".into(),
            decision: "allow".into(),
        });
        assert!(fixture.load("first", "edit.txt").can_accept);
        fixture.event(AgentEventKind::ToolStarted {
            tool_call: write("next", "edit.txt", "after\n"),
        });
        assert!(!fixture.load("first", "edit.txt").can_accept);
        assert!(
            fixture.accept(&first).is_err(),
            "even equal bytes cannot accept a later operation under the old token"
        );
    }

    #[tokio::test]
    async fn missing_or_corrupt_evidence_never_becomes_a_full_review_document() {
        let mut fixture = Fixture::new();
        fs::write(fixture.workspace.join("edit.txt"), "before\n").unwrap();
        fixture.legacy_write("legacy", "after\n");
        assert!(load_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "legacy",
            "edit.txt"
        )
        .is_err());
        fixture.execute(write("exact", "edit.txt", "next\n")).await;
        let review = fixture.load("exact", "edit.txt");
        assert!(accept_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "exact",
            "edit.txt",
            "wrong-version"
        )
        .is_err());
        fixture.accept(&review).unwrap();
        let receipt_path = fixture.storage.join(DIRECTORY).join(receipt_name(&review));
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        record["payload"]["sequence"] = json!(999);
        fs::write(receipt_path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(load_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "exact",
            "edit.txt"
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(fixture.workspace.join("edit.txt")).unwrap(),
            "next\n"
        );
    }

    #[tokio::test]
    async fn latest_review_selects_the_file_revision_and_never_skips_newer_failed_or_pending_edits()
    {
        let mut fixture = Fixture::new();
        assert!(load_latest_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "edit.txt"
        )
        .unwrap()
        .is_none());
        fixture.execute(write("first", "edit.txt", "first\n")).await;
        fixture
            .execute(write("unrelated", "other.txt", "other\n"))
            .await;
        let first =
            load_latest_file_review(&fixture.storage, "session", &fixture.workspace, "edit.txt")
                .unwrap()
                .unwrap();
        assert_eq!(first.tool_call_id, "first");
        fixture.accept(&first).unwrap();
        assert!(
            load_latest_file_review(&fixture.storage, "session", &fixture.workspace, "edit.txt")
                .unwrap()
                .unwrap()
                .accepted
        );
        fixture
            .execute(write("second", "edit.txt", "second\n"))
            .await;
        let second =
            load_latest_file_review(&fixture.storage, "session", &fixture.workspace, "edit.txt")
                .unwrap()
                .unwrap();
        assert_eq!(second.tool_call_id, "second");
        assert!(!second.accepted && second.can_accept);
        fixture.event(AgentEventKind::ToolStarted {
            tool_call: write("pending", "edit.txt", "third\n"),
        });
        assert!(load_latest_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "edit.txt"
        )
        .is_err());
        fixture.event(AgentEventKind::ToolFinished {
            tool_call_id: "pending".into(),
            output: "failed".into(),
            is_error: true,
        });
        assert!(load_latest_file_review(
            &fixture.storage,
            "session",
            &fixture.workspace,
            "edit.txt"
        )
        .is_err());
        assert_eq!(
            fixture.load("second", "edit.txt").after.as_deref(),
            Some("second\n"),
            "explicit historical opening remains available"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn receipt_and_current_path_links_fail_closed_without_following_them() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let mut fixture = Fixture::new();
        fixture.execute(write("write", "edit.txt", "saved\n")).await;
        let review = fixture.load("write", "edit.txt");
        let outside = fixture._temporary.path().join("outside.txt");
        fs::write(&outside, "saved\n").unwrap();
        fs::remove_file(fixture.workspace.join("edit.txt")).unwrap();
        symlink(&outside, fixture.workspace.join("edit.txt")).unwrap();
        assert!(!fixture.load("write", "edit.txt").can_accept);
        assert!(fixture.accept(&review).is_err());
        fs::remove_file(fixture.workspace.join("edit.txt")).unwrap();
        fs::write(fixture.workspace.join("edit.txt"), "saved\n").unwrap();
        fixture.accept(&review).unwrap();
        let receipt = fixture.storage.join(DIRECTORY).join(receipt_name(&review));
        assert_eq!(
            fs::metadata(&receipt).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&receipt, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(fixture.accept(&review).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "saved\n");
    }
}
