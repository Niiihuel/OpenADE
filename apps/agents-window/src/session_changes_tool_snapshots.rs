//! Exact, bounded observations around one admitted native file mutation.
//! Stored outside the model transcript; capture failure never retries or denies a tool.
use super::*;
use crate::session_changes_editor::{Directory, WorkspaceGuard, require_private_file};
use async_trait::async_trait;
use openide_harness::{
    AgentError, ToolContext, ToolDefinition, ToolExecutor, ToolOutput, ToolOutputSender,
};

const DIRECTORY: &str = "session-tool-snapshots";
const MAX_SIDE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENCODED_BYTES: usize = 3 * 1024 * 1024;
const MAX_STORE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_STORE_FILES: usize = 4096;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum Content {
    Absent,
    Present { base64: String },
    Unavailable { reason: String },
}

impl Content {
    fn decode(&self) -> Result<Option<Vec<u8>>, String> {
        match self {
            Self::Absent => Ok(None),
            Self::Present { base64 } => STANDARD
                .decode(base64)
                .map(Some)
                .map_err(|_| "Captura de archivo inválida".into()),
            Self::Unavailable { reason } => Err(reason.clone()),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u8,
    session_id: String,
    workspace: PathBuf,
    call_id: String,
    call_digest: String,
    start_sequence: u64,
    start_digest: String,
    operation_id: String,
    after: bool,
    files: BTreeMap<String, Content>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSnapshot {
    sha256: String,
    payload: Snapshot,
}

pub(crate) fn with_file_snapshots(
    inner: Arc<dyn ToolExecutor>,
    storage_root: PathBuf,
    session_id: String,
    workspace: PathBuf,
) -> Arc<dyn ToolExecutor> {
    Arc::new(SnapshotTools {
        inner,
        storage_root,
        session_id,
        workspace,
    })
}

struct SnapshotTools {
    inner: Arc<dyn ToolExecutor>,
    storage_root: PathBuf,
    session_id: String,
    workspace: PathBuf,
}

impl SnapshotTools {
    async fn execute_observed(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        output: Option<ToolOutputSender>,
    ) -> Result<ToolOutput, AgentError> {
        let capture = if matches!(call.name.as_str(), "write_file" | "apply_patch") {
            let storage = self.storage_root.clone();
            let session = self.session_id.clone();
            let workspace = self.workspace.clone();
            let actual = context.workspace.clone();
            let call = call.clone();
            tokio::task::spawn_blocking(move || {
                Capture::begin(&storage, &session, &workspace, &actual, &call)
            })
            .await
            .ok()
            .and_then(Result::ok)
        } else {
            None
        };
        let result = match output {
            Some(output) => self.inner.execute_streaming(call, context, output).await,
            None => self.inner.execute(call, context).await,
        };
        if let Some(capture) = capture {
            if result.as_ref().is_ok_and(|result| !result.is_error) {
                // Never report an already completed write as failed because a
                // display sidecar could not be stored (that could repeat effects).
                let call = call.clone();
                let _ = tokio::task::spawn_blocking(move || capture.finish(&call)).await;
            }
        }
        result
    }
}

#[async_trait]
impl ToolExecutor for SnapshotTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.inner.definitions()
    }
    fn certifies_parallel_read(&self, name: &str) -> bool {
        self.inner.certifies_parallel_read(name)
    }
    fn certifies_plan_read(&self, name: &str) -> bool {
        self.inner.certifies_plan_read(name)
    }
    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        self.execute_observed(call, context, None).await
    }
    async fn execute_streaming(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        output: ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        self.execute_observed(call, context, Some(output)).await
    }
}

struct Capture {
    _guard: WorkspaceGuard,
    directory: Directory,
    storage_root: PathBuf,
    before: Snapshot,
}

impl Capture {
    fn begin(
        storage_root: &Path,
        session_id: &str,
        workspace: &Path,
        actual: &Path,
        call: &ToolCall,
    ) -> io::Result<Self> {
        baseline_path(storage_root, session_id)?;
        let workspace = workspace.canonicalize()?;
        if workspace != actual.canonicalize()? {
            return Err(invalid("La captura no corresponde a esta carpeta"));
        }
        let paths = call_paths(call, &[&workspace, actual]).map_err(|error| invalid(&error))?;
        if paths.is_empty() || paths.len() > MAX_TOOL_DIFF_FILES {
            return Err(invalid("La edición excede las rutas de captura"));
        }
        let guard = crate::session_changes_editor::lock_workspace(storage_root, &workspace)?;
        let StoredJournal::Events(events) = read_stored_journal(storage_root, session_id)? else {
            return Err(invalid("No hay admisión persistida para esta edición"));
        };
        validate_journal(session_id, &workspace, &events)?;
        let mut starts = events.iter().filter(|event| {
            matches!(&event.kind,
            AgentEventKind::ToolStarted { tool_call } if tool_call.id == call.id)
        });
        let start = starts
            .next()
            .ok_or_else(|| invalid("No hay admisión persistida para esta edición"))?;
        if starts.next().is_some()
            || !matches!(&start.kind, AgentEventKind::ToolStarted { tool_call } if tool_call == call)
            || events.iter().any(|event| matches!(&event.kind, AgentEventKind::ToolFinished { tool_call_id, .. } if tool_call_id == &call.id)) {
            return Err(invalid("La edición no tiene una admisión nueva y única"));
        }
        let directory = Directory::open_named(storage_root, DIRECTORY, true)?
            .ok_or_else(|| invalid("No se pudo abrir el registro de archivos"))?;
        let before_name = snapshot_name(session_id, &call.id, false)?;
        match directory.read(&before_name) {
            Ok(_) => {
                return Err(invalid(
                    "Este identificador de edición ya tiene una captura",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let before = Snapshot {
            version: 1,
            session_id: session_id.into(),
            workspace: workspace.clone(),
            call_id: call.id.clone(),
            call_digest: call_digest(call)?,
            start_sequence: start.sequence,
            start_digest: digest(&serde_json::to_vec(start).map_err(io::Error::other)?),
            operation_id: uuid::Uuid::new_v4().to_string(),
            after: false,
            files: observe(&workspace, paths.iter()),
        };
        persist(&directory, storage_root, &before)?;
        Ok(Self {
            _guard: guard,
            directory,
            storage_root: storage_root.into(),
            before,
        })
    }

    fn finish(self, call: &ToolCall) -> io::Result<()> {
        let mut files = observe(&self.before.workspace, self.before.files.keys());
        if call.name == "write_file" {
            // A concurrent external actor cannot turn different observed bytes
            // into the content the tool is known to have written.
            if let Some(expected) = call
                .arguments
                .get("content")
                .and_then(|value| value.as_str())
            {
                for file in files.values_mut() {
                    if file.decode().as_ref().ok().and_then(Option::as_deref)
                        != Some(expected.as_bytes())
                    {
                        *file = Content::Unavailable {
                            reason: "Dato no disponible: el archivo cambió después de la escritura"
                                .into(),
                        };
                    }
                }
            }
        } else if call.name == "apply_patch" {
            // Derive this operation's expected bytes from the observed BEFORE,
            // rather than attributing an external actor's later write to it.
            let baseline = Baseline {
                version: VERSION,
                session_id: self.before.session_id.clone(),
                workspace: self.before.workspace.clone(),
                captured_at_ms: 0,
                branch: String::new(),
                head: None,
                inventory_complete: false,
                files: BTreeMap::new(),
                directory_entries: BTreeMap::new(),
            };
            let replayed = self
                .before
                .files
                .iter()
                .map(|(path, content)| {
                    (
                        path.clone(),
                        ReplayedFile {
                            content: content.decode(),
                            last_sequence: self.before.start_sequence,
                            epoch: 0,
                        },
                    )
                })
                .collect();
            let expected = call
                .arguments
                .get("patch")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "Patch sin contenido".to_owned())
                .and_then(|patch| replay_patch(&baseline, &replayed, patch, 0));
            for (path, file) in &mut files {
                if !expected
                    .as_ref()
                    .ok()
                    .and_then(|expected| expected.get(path))
                    .is_some_and(|expected| file.decode().as_ref().ok() == Some(expected))
                {
                    *file = Content::Unavailable { reason: "Dato no disponible: el contenido posterior no coincide con la edición confirmada".into() };
                }
            }
        }
        persist(
            &self.directory,
            &self.storage_root,
            &Snapshot {
                files,
                after: true,
                ..self.before
            },
        )
    }
}

fn observe<'a>(
    workspace: &Path,
    paths: impl Iterator<Item = &'a String>,
) -> BTreeMap<String, Content> {
    let mut retained = 0usize;
    paths.map(|path| {
        let content = match read_workspace_file(workspace, path) {
            Ok(bytes) if retained + bytes.len() <= MAX_SIDE_BYTES => {
                retained += bytes.len();
                Content::Present { base64: STANDARD.encode(bytes) }
            },
            Ok(_) => Content::Unavailable { reason: "Dato no disponible: se alcanzó el límite de la captura de edición".into() },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Content::Absent,
            Err(_) => Content::Unavailable { reason: "Dato no disponible: archivo no legible, enlazado, modificado o demasiado grande".into() },
        };
        (path.clone(), content)
    }).collect()
}

fn call_digest(call: &ToolCall) -> io::Result<String> {
    serde_json::to_vec(call)
        .map(|bytes| digest(&bytes))
        .map_err(io::Error::other)
}

fn snapshot_name(session: &str, call: &str, after: bool) -> io::Result<String> {
    baseline_path(Path::new("."), session)?;
    if call.is_empty() || call.len() > 1024 || call.chars().any(char::is_control) {
        return Err(invalid("Identificador de edición inválido"));
    }
    Ok(format!(
        "{session}.{}.{}.json",
        digest(call.as_bytes()),
        if after { "after" } else { "before" }
    ))
}

fn persist(directory: &Directory, storage_root: &Path, snapshot: &Snapshot) -> io::Result<()> {
    let payload = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
    let encoded =
        serde_json::to_vec(&serde_json::json!({ "sha256": digest(&payload), "payload": snapshot }))
            .map_err(io::Error::other)?;
    if encoded.len() > MAX_ENCODED_BYTES {
        return Err(invalid("Captura de edición demasiado grande"));
    }
    // A display feature may not grow its store without bound. No user files
    // are removed to free space; older captures remain immutable.
    let mut total = encoded.len() as u64;
    for (index, entry) in fs::read_dir(storage_root.join(DIRECTORY))?.enumerate() {
        if index + 1 >= MAX_STORE_FILES {
            return Err(invalid("Registro de ediciones lleno"));
        }
        total = total.saturating_add(entry?.metadata()?.len());
        if total > MAX_STORE_BYTES {
            return Err(invalid("Registro de ediciones lleno"));
        }
    }
    let temporary = format!(".{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file = directory.create(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        directory.publish(
            &temporary,
            &snapshot_name(&snapshot.session_id, &snapshot.call_id, snapshot.after)?,
        )?;
        directory.sync()
    })();
    let _ = directory.remove(&temporary);
    result
}

fn read(
    directory: &Directory,
    session: &str,
    root: &Path,
    recorded_root: &Path,
    call: &ToolCall,
    start: &AgentEvent,
    after: bool,
) -> io::Result<Option<Snapshot>> {
    let file = match directory.read(&snapshot_name(session, &call.id, after)?) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    require_private_file(&metadata)?;
    if !metadata.is_file() || metadata.len() > MAX_ENCODED_BYTES as u64 {
        return Err(invalid("Captura de edición inválida"));
    }
    let mut bytes = Vec::new();
    file.take((MAX_ENCODED_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(invalid("Captura de edición demasiado grande"));
    }
    let stored: StoredSnapshot = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let value = stored.payload;
    let paths = call_paths(call, &[root, recorded_root]).map_err(|error| invalid(&error))?;
    if value.version != 1
        || value.session_id != session
        || value.workspace != root
        || value.call_id != call.id
        || value.call_digest != call_digest(call)?
        || value.start_sequence != start.sequence
        || value.start_digest != digest(&serde_json::to_vec(start).map_err(io::Error::other)?)
        || value.after != after
        || value.operation_id.len() > 64
        || value.files.keys().cloned().collect::<BTreeSet<_>>() != paths
        || paths.len() > MAX_TOOL_DIFF_FILES
        || digest(&serde_json::to_vec(&value).map_err(io::Error::other)?) != stored.sha256
    {
        return Err(invalid("La captura no corresponde a esta edición"));
    }
    let mut retained = 0;
    for (path, content) in &value.files {
        if normalized_path(path).map_err(|error| invalid(&error))? != *path {
            return Err(invalid("Ruta de captura inválida"));
        }
        match content {
            Content::Present { base64 } => {
                if base64.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
                    return Err(invalid("Archivo capturado demasiado grande"));
                }
                let bytes = STANDARD.decode(base64).map_err(io::Error::other)?;
                retained += bytes.len();
                if bytes.len() > MAX_FILE_BYTES || retained > MAX_SIDE_BYTES {
                    return Err(invalid("Captura de edición demasiado grande"));
                }
            }
            Content::Unavailable { reason } if reason.len() > 256 => {
                return Err(invalid("Metadatos de captura inválidos"));
            }
            _ => {}
        }
    }
    Ok(Some(value))
}

pub(super) fn load_transition(
    storage: &Path,
    session: &str,
    root: &Path,
    recorded_root: &Path,
    call: &ToolCall,
    start: &AgentEvent,
    sequence: u64,
) -> io::Result<Option<ToolTransition>> {
    let Some(directory) = Directory::open_named(storage, DIRECTORY, false)? else {
        return Ok(None);
    };
    let Some(before) = read(&directory, session, root, recorded_root, call, start, false)? else {
        return Ok(None);
    };
    let Some(after) = read(&directory, session, root, recorded_root, call, start, true)? else {
        // A real BEFORE supersedes the initial inventory. Losing the AFTER
        // must not silently restore a potentially different global baseline.
        let after = before
            .files
            .keys()
            .map(|path| {
                (
                    path.clone(),
                    Err("Dato no disponible: no se pudo conservar la versión posterior de esta edición".into()),
                )
            })
            .collect();
        return Ok(Some(ToolTransition {
            sequence,
            before: before
                .files
                .into_iter()
                .map(|(path, content)| (path, content.decode()))
                .collect(),
            after,
            renames: BTreeMap::new(),
            unavailable: None,
            allow_after_snapshot: false,
        }));
    };
    if before.operation_id != after.operation_id {
        return Err(invalid("Las capturas pertenecen a operaciones diferentes"));
    }
    Ok(Some(ToolTransition {
        sequence,
        before: before
            .files
            .into_iter()
            .map(|(path, content)| (path, content.decode()))
            .collect(),
        after: after
            .files
            .into_iter()
            .map(|(path, content)| (path, content.decode()))
            .collect(),
        renames: patch_renames(call),
        unavailable: None,
        allow_after_snapshot: call.name == "write_file",
    }))
}

pub(super) fn remove_session(storage: &Path, session: &str) -> io::Result<()> {
    baseline_path(storage, session)?;
    let Some(directory) = Directory::open_named(storage, DIRECTORY, false)? else {
        return Ok(());
    };
    let prefix = format!("{session}.");
    for entry in fs::read_dir(storage.join(DIRECTORY))?.take(MAX_STORE_FILES + 1) {
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
    use openide_harness::LocalTools;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    struct Fixture {
        _temp: tempfile::TempDir,
        storage: PathBuf,
        workspace: PathBuf,
        events: Vec<AgentEvent>,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let storage = temp.path().join("store");
            let workspace = temp.path().join("workspace");
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
                _temp: temp,
                storage,
                workspace,
                events,
            }
        }
        fn executor(&self) -> Arc<dyn ToolExecutor> {
            with_file_snapshots(
                Arc::new(LocalTools::new().with_write_access(true)),
                self.storage.clone(),
                "session".into(),
                self.workspace.clone(),
            )
        }
        fn event(&mut self, kind: AgentEventKind) {
            self.events.push(AgentEvent::new(
                "session",
                self.events.len() as u64 + 1,
                kind,
            ));
        }
        fn persist(&self) {
            let mut bytes = Vec::new();
            for event in &self.events {
                serde_json::to_writer(&mut bytes, event).unwrap();
                bytes.push(b'\n');
            }
            fs::write(self.storage.join("session.jsonl"), bytes).unwrap();
        }
        async fn execute(&mut self, call: ToolCall) -> ToolOutput {
            self.event(AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            });
            self.persist();
            let result = self
                .executor()
                .execute(
                    &call,
                    &ToolContext {
                        workspace: self.workspace.clone(),
                        cancel: CancellationToken::new(),
                    },
                )
                .await
                .unwrap_or_else(|error| ToolOutput::error(error.to_string()));
            self.event(AgentEventKind::ToolFinished {
                tool_call_id: call.id,
                output: result.content.clone(),
                is_error: result.is_error,
            });
            self.persist();
            result
        }
        fn load(&self, call: &str) -> crate::session_tool_diff::SessionToolDiff {
            crate::session_tool_diff::load_stored_tool_diff(
                &self.storage,
                "session",
                &self.workspace,
                call,
            )
            .unwrap()
        }
    }

    fn write(id: &str, path: &str, content: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "write_file".into(),
            arguments: json!({"path":path, "content":content}),
        }
    }

    fn lines(file: &SessionFileChange) -> String {
        file.file
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    struct EditingModel(std::sync::atomic::AtomicUsize);

    #[async_trait]
    impl openide_harness::ModelBackend for EditingModel {
        fn model_id(&self) -> &str {
            "fixture/file-edit"
        }
        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([8; 32])
        }
        async fn complete(
            &self,
            messages: &[openide_core::AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<openide_harness::ModelOutput, AgentError> {
            assert!(
                messages
                    .iter()
                    .all(|message| !message.content.contains("private before snapshot")),
                "snapshot evidence is display data, never additional model input"
            );
            Ok(
                if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    openide_harness::ModelOutput {
                        content: String::new(),
                        tool_calls: vec![write(
                            "admitted-write",
                            "edit.txt",
                            "saved through runner\n",
                        )],
                    }
                } else {
                    openide_harness::ModelOutput {
                        content: "Complete".into(),
                        tool_calls: vec![],
                    }
                },
            )
        }
    }

    #[test]
    fn runner_preparation_persists_admission_before_real_file_snapshot_and_execution() {
        use openide_core::{AgentRunRequest, SessionStore, TurnMode};
        use openide_harness::AgentRunner;
        let fixture = Fixture::new();
        fs::write(
            fixture.workspace.join("edit.txt"),
            "private before snapshot\n",
        )
        .unwrap();
        let store = Arc::new(SessionStore::open(&fixture.storage).unwrap());
        let session = store.create(&fixture.workspace, "native").unwrap();
        let admission = store
            .admit_native_turn(
                &session.id,
                "edit-turn",
                "Update edit.txt",
                "fixture/file-edit",
                TurnMode::Build,
            )
            .unwrap();
        let prior = Arc::new(store.load(&session.id).unwrap().unwrap().events);
        let runner = AgentRunner::new(
            Arc::new(EditingModel(std::sync::atomic::AtomicUsize::new(0))),
            Arc::new(LocalTools::new().with_write_access(true)),
        )
        .with_event_journal(store.clone())
        .with_native_runtime_identity("fixture", "https://fixture.invalid/v1")
        .with_tool_preparation(Arc::new(
            crate::environment_runtime::EnvironmentToolPreparation::new(
                fixture.storage.clone(),
                session.id.clone(),
                prior,
                vec![],
                None,
            ),
        ));
        let handle = runner.start_with_lease(
            AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: admission.sequence,
                workspace: fixture.workspace.clone(),
                history: vec![],
                prompt: "Update edit.txt".into(),
                mode: TurnMode::Build,
            },
            store.try_acquire_native_lease(&session.id).unwrap(),
        );
        let mut complete = false;
        while let Ok(event) = handle.events.recv_timeout(Duration::from_secs(10)) {
            assert!(
                !matches!(&event.kind, AgentEventKind::TurnFailed { .. }),
                "{event:?}"
            );
            if matches!(&event.kind, AgentEventKind::TurnCompleted { .. }) {
                complete = true;
                break;
            }
        }
        assert!(complete, "native runner did not complete");
        assert!(handle.journal_error().is_none());
        for after in [false, true] {
            assert!(
                fixture
                    .storage
                    .join(DIRECTORY)
                    .join(snapshot_name(&session.id, "admitted-write", after).unwrap())
                    .is_file(),
                "EnvironmentToolPreparation must install the recording executor"
            );
        }
        let diff = crate::session_tool_diff::load_stored_tool_diff(
            &fixture.storage,
            &session.id,
            &fixture.workspace,
            "admitted-write",
        )
        .unwrap();
        assert!(!diff.limited, "{diff:?}");
        assert!(lines(&diff.files[0]).contains("-private before snapshot"));
        assert!(lines(&diff.files[0]).contains("+saved through runner"));
        assert_eq!(
            fs::read_to_string(fixture.workspace.join("edit.txt")).unwrap(),
            "saved through runner\n"
        );
    }

    #[tokio::test]
    async fn captures_only_edited_targets_when_initial_inventory_is_exhausted() {
        let mut fixture = Fixture::new();
        fs::create_dir(fixture.workspace.join("a-crowded")).unwrap();
        for index in 0..MAX_ENTRIES + 1 {
            fs::write(fixture.workspace.join(format!("a-crowded/{index:05}")), "").unwrap();
        }
        fs::write(fixture.workspace.join("z-edit.txt"), "first\nold line\n").unwrap();
        assert!(
            matches!(capture_baseline(&fixture.storage, "session", &fixture.workspace, &fixture.events).unwrap(), BaselineCapture::Created(info) if !info.inventory_complete)
        );
        let result = fixture
            .execute(write("edit", "z-edit.txt", "first\nnew line\n"))
            .await;
        assert!(!result.is_error);
        let diff = fixture.load("edit");
        assert!(!diff.limited, "{:?}", diff.coverage_notes);
        assert_eq!(
            (diff.files[0].file.additions, diff.files[0].file.deletions),
            (1, 1)
        );
        assert!(lines(&diff.files[0]).contains("-old line"));
        assert!(lines(&diff.files[0]).contains("+new line"));
        assert_eq!(
            fs::read_dir(fixture.storage.join(DIRECTORY))
                .unwrap()
                .count(),
            2
        );
        fs::create_dir_all(fixture.workspace.join("new/sub")).unwrap();
        assert!(
            !fixture
                .execute(write("new", "new/sub/file.txt", "created\n"))
                .await
                .is_error
        );
        let diff = fixture.load("new");
        assert_eq!(diff.files[0].file.status, "Nuevo");
        assert_eq!(
            (diff.files[0].file.additions, diff.files[0].file.deletions),
            (1, 0)
        );
    }

    #[tokio::test]
    async fn every_edit_uses_immediate_before_and_reopens_without_current_disk_or_initial_baseline()
    {
        let mut fixture = Fixture::new();
        fs::write(fixture.workspace.join("edit.txt"), "original\n").unwrap();
        assert!(
            !fixture
                .execute(write("first", "edit.txt", "version one\n"))
                .await
                .is_error
        );
        // Simulates an explicit, separate editor save between two admissions.
        fs::write(fixture.workspace.join("edit.txt"), "manual before second\n").unwrap();
        assert!(
            !fixture
                .execute(write("second", "edit.txt", "version two\n"))
                .await
                .is_error
        );
        fs::remove_file(fixture.workspace.join("edit.txt")).unwrap();
        let first = fixture.load("first");
        let second = fixture.load("second");
        assert!(!first.limited && !second.limited, "{first:?}\n{second:?}");
        assert!(lines(&first.files[0]).contains("-original"));
        assert!(lines(&first.files[0]).contains("+version one"));
        assert!(lines(&second.files[0]).contains("-manual before second"));
        assert!(lines(&second.files[0]).contains("+version two"));
        remove_baseline(&fixture.storage, "session").unwrap();
        assert_eq!(
            fs::read_dir(fixture.storage.join(DIRECTORY))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn actual_patch_tracks_update_rename_delete_and_creation() {
        let mut fixture = Fixture::new();
        fs::write(fixture.workspace.join("old.txt"), "old line\n").unwrap();
        fs::write(fixture.workspace.join("delete.txt"), "removed\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-old line\n+new line\n*** Delete File: delete.txt\n*** Add File: added.txt\n+added\n*** End Patch\n";
        let result = fixture
            .execute(ToolCall {
                id: "patch".into(),
                name: "apply_patch".into(),
                arguments: json!({"patch":patch}),
            })
            .await;
        assert!(!result.is_error, "{}", result.content);
        let diff = fixture.load("patch");
        assert!(!diff.limited, "{diff:?}");
        assert_eq!(diff.files.len(), 3);
        let moved = diff
            .files
            .iter()
            .find(|file| file.file.path == "moved.txt")
            .unwrap();
        assert_eq!(moved.file.old_path.as_deref(), Some("old.txt"));
        assert_eq!((moved.file.additions, moved.file.deletions), (1, 1));
        assert_eq!(
            diff.files
                .iter()
                .find(|file| file.file.path == "added.txt")
                .unwrap()
                .file
                .status,
            "Nuevo"
        );
        assert_eq!(
            diff.files
                .iter()
                .find(|file| file.file.path == "delete.txt")
                .unwrap()
                .file
                .status,
            "Eliminado"
        );
    }

    struct ExternalWriteAfterTool;

    #[async_trait]
    impl ToolExecutor for ExternalWriteAfterTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            LocalTools::new().with_write_access(true).definitions()
        }

        async fn execute(
            &self,
            call: &ToolCall,
            context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            let result = LocalTools::new()
                .with_write_access(true)
                .execute(call, context)
                .await?;
            assert!(!result.is_error);
            // A non-cooperating writer changes this private fixture after
            // the native mutation, before the wrapper observes its result.
            fs::write(context.workspace.join("edit.txt"), "external change\n").unwrap();
            Ok(result)
        }
    }

    #[tokio::test]
    async fn external_after_write_is_not_attributed_to_the_confirmed_tool() {
        for call in [
            write("edit", "edit.txt", "intended change\n"),
            ToolCall {
                id: "edit".into(),
                name: "apply_patch".into(),
                arguments: json!({"patch":"*** Begin Patch\n*** Update File: edit.txt\n@@\n-original\n+intended change\n*** End Patch\n"}),
            },
        ] {
            let mut fixture = Fixture::new();
            fs::write(fixture.workspace.join("edit.txt"), "original\n").unwrap();
            fixture.event(AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            });
            fixture.persist();
            let result = with_file_snapshots(
                Arc::new(ExternalWriteAfterTool),
                fixture.storage.clone(),
                "session".into(),
                fixture.workspace.clone(),
            )
            .execute(
                &call,
                &ToolContext {
                    workspace: fixture.workspace.clone(),
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
            assert!(!result.is_error, "capture must not replace tool success");
            fixture.event(AgentEventKind::ToolFinished {
                tool_call_id: call.id,
                output: result.content,
                is_error: false,
            });
            fixture.persist();
            let diff = fixture.load("edit");
            assert!(diff.limited);
            assert_eq!(diff.files.len(), 1);
            assert!(diff.files[0].unavailable.is_some());
            assert!(diff.files[0].file.lines.is_empty());
            assert_eq!(
                (diff.files[0].file.additions, diff.files[0].file.deletions),
                (0, 0),
                "a concurrent actor's bytes cannot become an AI diff"
            );
        }
    }

    #[tokio::test]
    async fn failed_tool_and_reused_identifier_cannot_claim_a_new_snapshot() {
        let mut fixture = Fixture::new();
        fs::write(fixture.workspace.join("edit.txt"), "initial\n").unwrap();
        let failed = fixture.execute(ToolCall { id: "failed".into(), name: "apply_patch".into(), arguments: json!({"patch":"*** Begin Patch\n*** Add File: edit.txt\n+conflict\n*** End Patch\n"}) }).await;
        assert!(failed.is_error);
        assert!(
            !fixture
                .storage
                .join(DIRECTORY)
                .join(snapshot_name("session", "failed", true).unwrap())
                .exists()
        );
        assert!(fixture.load("failed").files.is_empty());
        let call = write("same", "edit.txt", "first\n");
        assert!(!fixture.execute(call.clone()).await.is_error);
        let before = fs::read(
            fixture
                .storage
                .join(DIRECTORY)
                .join(snapshot_name("session", "same", false).unwrap()),
        )
        .unwrap();
        assert!(
            !fixture
                .execute(write("same", "edit.txt", "second\n"))
                .await
                .is_error
        );
        assert_eq!(
            fs::read(
                fixture
                    .storage
                    .join(DIRECTORY)
                    .join(snapshot_name("session", "same", false).unwrap())
            )
            .unwrap(),
            before
        );
        assert!(fixture.load("same").unavailable.is_some());
        let mut replaced_start = fixture.events[3].clone();
        replaced_start.timestamp_ms += 1;
        assert!(
            load_transition(
                &fixture.storage,
                "session",
                &fixture.workspace,
                &fixture.workspace,
                &call,
                &replaced_start,
                5
            )
            .is_err(),
            "a reused call ID cannot bind a different admitted event"
        );
        let mut other = call;
        other.arguments["content"] = json!("tampered");
        assert!(
            load_transition(
                &fixture.storage,
                "session",
                &fixture.workspace,
                &fixture.workspace,
                &other,
                &fixture.events[3],
                3
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn unreadable_snapshot_store_fails_open_without_changing_tool_result() {
        let mut fixture = Fixture::new();
        fs::write(fixture.storage.join(DIRECTORY), "not a directory").unwrap();
        let output = fixture.execute(write("write", "new.txt", "saved\n")).await;
        assert_eq!(output, ToolOutput::success("file written"));
        assert_eq!(
            fs::read_to_string(fixture.workspace.join("new.txt")).unwrap(),
            "saved\n"
        );
    }

    #[tokio::test]
    async fn incomplete_snapshot_pair_never_falls_back_to_a_different_initial_version() {
        let mut fixture = Fixture::new();
        fs::write(fixture.workspace.join("edit.txt"), "initial inventory\n").unwrap();
        capture_baseline(
            &fixture.storage,
            "session",
            &fixture.workspace,
            &fixture.events,
        )
        .unwrap();
        fs::write(fixture.workspace.join("edit.txt"), "actual before\n").unwrap();
        assert!(
            !fixture
                .execute(write("edit", "edit.txt", "confirmed after\n"))
                .await
                .is_error
        );
        fs::remove_file(
            fixture
                .storage
                .join(DIRECTORY)
                .join(snapshot_name("session", "edit", true).unwrap()),
        )
        .unwrap();
        let diff = fixture.load("edit");
        assert!(diff.limited);
        assert_eq!(diff.files.len(), 1);
        assert!(
            diff.files[0]
                .unavailable
                .as_ref()
                .unwrap()
                .contains("versión posterior")
        );
        assert!(diff.files[0].file.lines.is_empty());
        assert_eq!(
            fs::read_to_string(fixture.workspace.join("edit.txt")).unwrap(),
            "confirmed after\n",
            "missing display evidence does not undo a successful mutation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshots_reject_linked_paths_and_private_store_tampering() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let mut fixture = Fixture::new();
        let outside = fixture._temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "external private bytes").unwrap();
        symlink(&outside, fixture.workspace.join("linked")).unwrap();
        let observed = observe(&fixture.workspace, ["linked/secret.txt".to_owned()].iter());
        assert!(observed["linked/secret.txt"].decode().is_err());
        assert!(
            !serde_json::to_string(&observed)
                .unwrap()
                .contains("external private bytes")
        );
        assert!(
            !fixture
                .execute(write("ok", "safe.txt", "safe\n"))
                .await
                .is_error
        );
        let before = fixture
            .storage
            .join(DIRECTORY)
            .join(snapshot_name("session", "ok", false).unwrap());
        assert_eq!(
            fs::metadata(&before).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&before, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            crate::session_tool_diff::load_stored_tool_diff(
                &fixture.storage,
                "session",
                &fixture.workspace,
                "ok"
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "external private bytes"
        );
    }
}
