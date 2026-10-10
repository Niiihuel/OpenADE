//! Workspace-scoped launch actions. Reading manifests only suggests commands;
//! executing an action is exclusively a host action after an explicit click.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SendError, Sender},
    thread,
};

const MAX_ACTIONS: usize = 32;
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_MANIFEST_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ProjectAction {
    pub id: String,
    pub label: String,
    pub command: String,
}

#[derive(Clone, Debug, Default)]
pub struct ProjectActionsState {
    pub workspace: Option<PathBuf>,
    pub actions: Vec<ProjectAction>,
    pub presets: Vec<ProjectAction>,
    pub loading: bool,
    pub saving: bool,
    pub editor_open: bool,
    pub editing_id: Option<String>,
    pub label_draft: String,
    pub command_draft: String,
    pub error: Option<String>,
}

impl ProjectActionsState {
    pub fn begin_new(&mut self) {
        self.editor_open = true;
        self.editing_id = None;
        self.label_draft.clear();
        self.command_draft.clear();
        self.error = None;
    }
    pub fn begin_edit(&mut self, id: &str) -> bool {
        let Some(action) = self.actions.iter().find(|action| action.id == id) else {
            return false;
        };
        self.editor_open = true;
        self.editing_id = Some(action.id.clone());
        self.label_draft = action.label.clone();
        self.command_draft = action.command.clone();
        self.error = None;
        true
    }
    pub fn cancel_edit(&mut self) {
        self.editor_open = false;
        self.editing_id = None;
        self.label_draft.clear();
        self.command_draft.clear();
        self.error = None;
    }
    pub fn save_draft(&mut self) -> Result<(), String> {
        let label = self.label_draft.trim().to_owned();
        let command = self.command_draft.trim().to_owned();
        let id = self
            .editing_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let action = ProjectAction { id, label, command };
        validate_action(&action).map_err(|error| error.to_string())?;
        if let Some(editing) = &self.editing_id {
            let Some(previous) = self.actions.iter_mut().find(|action| &action.id == editing)
            else {
                return Err("La acción ya no existe. Crea una nueva.".into());
            };
            *previous = action;
        } else {
            if self.actions.len() >= MAX_ACTIONS {
                return Err("Puedes guardar hasta 32 acciones por proyecto.".into());
            }
            self.actions.push(action);
        }
        self.cancel_edit();
        Ok(())
    }
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.actions.len();
        self.actions.retain(|action| action.id != id);
        if self.editing_id.as_deref() == Some(id) {
            self.cancel_edit();
        }
        self.actions.len() != before
    }
    pub fn add_preset(&mut self, id: &str) -> Result<(), String> {
        let Some(preset) = self.presets.iter().find(|preset| preset.id == id).cloned() else {
            return Err("La sugerencia ya no está disponible.".into());
        };
        if self
            .actions
            .iter()
            .any(|action| action.command == preset.command)
        {
            return Ok(());
        }
        if self.actions.len() >= MAX_ACTIONS {
            return Err("Puedes guardar hasta 32 acciones por proyecto.".into());
        }
        self.actions.push(ProjectAction {
            id: uuid::Uuid::new_v4().to_string(),
            ..preset
        });
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProjectActionsSnapshot {
    pub actions: Vec<ProjectAction>,
    pub presets: Vec<ProjectAction>,
}

#[derive(Clone, Debug)]
pub enum ProjectActionsOperation {
    Load,
    Save(Vec<ProjectAction>),
}

#[derive(Clone, Debug)]
pub struct ProjectActionsRequest {
    pub data_root: PathBuf,
    pub workspace: PathBuf,
    pub generation: u64,
    pub operation: ProjectActionsOperation,
}

#[derive(Debug)]
pub struct ProjectActionsResponse {
    pub workspace: PathBuf,
    pub generation: u64,
    pub result: Result<ProjectActionsSnapshot, String>,
}

pub struct ProjectActionsWorker {
    request_tx: Sender<ProjectActionsRequest>,
    result_rx: Receiver<ProjectActionsResponse>,
}

impl ProjectActionsWorker {
    pub fn spawn() -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel::<ProjectActionsRequest>();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-project-actions".into())
            .spawn(move || {
                // Persist every user edit in request order, even across projects.
                while let Ok(request) = request_rx.recv() {
                    let result = match request.operation {
                        ProjectActionsOperation::Load => {
                            load(&request.data_root, &request.workspace)
                        }
                        ProjectActionsOperation::Save(actions) => {
                            save(&request.data_root, &request.workspace, &actions).map(|()| {
                                ProjectActionsSnapshot {
                                    actions,
                                    presets: discover_presets(&request.workspace),
                                }
                            })
                        }
                    }
                    .map_err(|error| error.to_string());
                    if result_tx
                        .send(ProjectActionsResponse {
                            workspace: request.workspace,
                            generation: request.generation,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            request_tx,
            result_rx,
        })
    }
    pub fn submit(
        &self,
        request: ProjectActionsRequest,
    ) -> Result<(), SendError<ProjectActionsRequest>> {
        self.request_tx.send(request)
    }
    pub fn poll(&self) -> Option<ProjectActionsResponse> {
        self.result_rx.try_recv().ok()
    }
}

#[derive(Serialize, Deserialize)]
struct StoredActions {
    version: u32,
    workspace: PathBuf,
    actions: Vec<ProjectAction>,
}

fn validate_action(action: &ProjectAction) -> io::Result<()> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    if action.id.is_empty()
        || action.id.len() > 100
        || !action
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(invalid("El identificador de la acción no es válido."));
    }
    if action.label.trim().is_empty()
        || action.label.chars().count() > 64
        || action.label.chars().any(char::is_control)
    {
        return Err(invalid("Escribe un nombre de hasta 64 caracteres."));
    }
    if action.command.trim().is_empty()
        || action.command.len() > 4096
        || action.command.chars().any(char::is_control)
    {
        return Err(invalid(
            "Escribe un comando en una sola línea, de hasta 4096 caracteres. Puedes unir comandos con &&.",
        ));
    }
    Ok(())
}

fn validate_actions(actions: &[ProjectAction]) -> io::Result<()> {
    if actions.len() > MAX_ACTIONS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Demasiadas acciones guardadas.",
        ));
    }
    let mut ids = HashSet::new();
    for action in actions {
        validate_action(action)?;
        if !ids.insert(&action.id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Hay acciones con identificadores duplicados.",
            ));
        }
    }
    Ok(())
}

fn file_path(data_root: &Path, workspace: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let workspace = workspace.canonicalize()?;
    if !workspace.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "El proyecto no es una carpeta.",
        ));
    }
    let digest = Sha256::digest(workspace.as_os_str().as_encoded_bytes());
    Ok((
        data_root
            .join("project-actions")
            .join(format!("{digest:x}.json")),
        workspace,
    ))
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "El archivo no es un documento regular.",
        ));
    }
    if metadata.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "El archivo supera el tamaño permitido.",
        ));
    }
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "El archivo supera el tamaño permitido.",
        ));
    }
    Ok(bytes)
}

pub fn load(data_root: &Path, workspace: &Path) -> io::Result<ProjectActionsSnapshot> {
    let (path, canonical) = file_path(data_root, workspace)?;
    let actions = match read_bounded(&path, MAX_FILE_BYTES) {
        Ok(bytes) => {
            let stored: StoredActions = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if stored.version != 1 || stored.workspace != canonical {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Las acciones guardadas no corresponden a este proyecto.",
                ));
            }
            validate_actions(&stored.actions)?;
            stored.actions
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    Ok(ProjectActionsSnapshot {
        actions,
        presets: discover_presets(workspace),
    })
}

pub fn save(data_root: &Path, workspace: &Path, actions: &[ProjectAction]) -> io::Result<()> {
    validate_actions(actions)?;
    let (destination, canonical) = file_path(data_root, workspace)?;
    let parent = destination.parent().expect("actions directory");
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(&StoredActions {
        version: 1,
        workspace: canonical,
        actions: actions.to_vec(),
    })?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Las acciones superan el tamaño permitido.",
        ));
    }
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn discover_presets(workspace: &Path) -> Vec<ProjectAction> {
    let mut presets = Vec::new();
    if let Ok(bytes) = read_bounded(&workspace.join("Cargo.toml"), MAX_MANIFEST_BYTES) {
        let text = String::from_utf8_lossy(&bytes);
        if text
            .lines()
            .any(|line| matches!(line.trim(), "[package]" | "[workspace]"))
        {
            for (id, label, command) in [
                ("cargo-build", "Compilar", "cargo build"),
                ("cargo-test", "Probar", "cargo test"),
            ] {
                presets.push(ProjectAction {
                    id: id.into(),
                    label: label.into(),
                    command: command.into(),
                });
            }
        }
    }
    if let Some(package) = read_bounded(&workspace.join("package.json"), MAX_MANIFEST_BYTES)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        let declared = package
            .get("packageManager")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let declared_manager = declared.split('@').next().unwrap_or_default();
        let manager = if matches!(declared_manager, "npm" | "pnpm" | "yarn" | "bun") {
            declared_manager
        } else if workspace.join("pnpm-lock.yaml").is_file() {
            "pnpm"
        } else if workspace.join("yarn.lock").is_file() {
            "yarn"
        } else if workspace.join("bun.lock").is_file() || workspace.join("bun.lockb").is_file() {
            "bun"
        } else {
            "npm"
        };
        if let Some(scripts) = package.get("scripts").and_then(|value| value.as_object()) {
            for (script, label) in [
                ("dev", "Desarrollo"),
                ("start", "Ejecutar"),
                ("build", "Compilar"),
                ("test", "Probar"),
            ] {
                if scripts
                    .get(script)
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    presets.push(ProjectAction {
                        id: format!("package-{script}"),
                        label: label.into(),
                        command: format!("{manager} run {script}"),
                    });
                }
            }
        }
    }
    presets
}

#[cfg(test)]
mod tests {
    use super::*;
    fn action() -> ProjectAction {
        ProjectAction {
            id: "build".into(),
            label: "Compilar".into(),
            command: "cargo build".into(),
        }
    }

    #[test]
    fn saved_actions_are_isolated_by_workspace_and_survive_edit_and_delete() {
        let root = tempfile::tempdir().unwrap();
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        save(root.path(), one.path(), &[action()]).unwrap();
        assert_eq!(
            load(root.path(), one.path()).unwrap().actions,
            vec![action()]
        );
        assert!(load(root.path(), two.path()).unwrap().actions.is_empty());
        let mut state = ProjectActionsState {
            actions: vec![action()],
            ..Default::default()
        };
        assert!(state.begin_edit("build"));
        state.command_draft = "cargo build --release".into();
        state.save_draft().unwrap();
        save(root.path(), one.path(), &state.actions).unwrap();
        assert_eq!(
            load(root.path(), one.path()).unwrap().actions[0].command,
            "cargo build --release"
        );
        assert!(state.remove("build"));
        save(root.path(), one.path(), &state.actions).unwrap();
        assert!(load(root.path(), one.path()).unwrap().actions.is_empty());
    }

    #[test]
    fn manifest_detection_only_proposes_fixed_script_commands_and_never_runs_them() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        fs::write(project.path().join("package.json"), r#"{"packageManager":"pnpm@9","scripts":{"dev":"touch should-not-exist","build":"$(touch should-not-exist)","strange;command":"echo bad"}}"#).unwrap();
        let snapshot = load(root.path(), project.path()).unwrap();
        assert!(snapshot.actions.is_empty());
        assert_eq!(
            snapshot
                .presets
                .iter()
                .map(|action| action.command.as_str())
                .collect::<Vec<_>>(),
            vec![
                "cargo build",
                "cargo test",
                "pnpm run dev",
                "pnpm run build"
            ]
        );
        assert!(!project.path().join("should-not-exist").exists());
        assert!(!root.path().join("project-actions").exists());
        fs::write(project.path().join("pnpm-lock.yaml"), "").unwrap();
        fs::write(
            project.path().join("package.json"),
            r#"{"packageManager":"npm@10","scripts":{"build":"echo build"}}"#,
        )
        .unwrap();
        assert!(discover_presets(project.path())
            .iter()
            .any(|preset| preset.command == "npm run build"));
    }

    #[test]
    fn invalid_commands_and_corrupt_scope_do_not_replace_saved_actions() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        save(root.path(), project.path(), &[action()]).unwrap();
        let invalid = ProjectAction {
            command: "bad\0command".into(),
            ..action()
        };
        assert!(save(root.path(), project.path(), &[invalid]).is_err());
        assert_eq!(
            load(root.path(), project.path()).unwrap().actions,
            vec![action()]
        );
        let (path, _) = file_path(root.path(), project.path()).unwrap();
        fs::write(path, r#"{"version":1,"workspace":"/wrong","actions":[]}"#).unwrap();
        assert!(load(root.path(), project.path()).is_err());
    }

    #[test]
    fn worker_echoes_scope_and_generation_and_persists_before_replying() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let worker = ProjectActionsWorker::spawn().unwrap();
        worker
            .submit(ProjectActionsRequest {
                data_root: root.path().into(),
                workspace: project.path().into(),
                generation: 23,
                operation: ProjectActionsOperation::Save(vec![action()]),
            })
            .unwrap();
        let response = worker
            .result_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(response.workspace, project.path());
        assert_eq!(response.generation, 23);
        assert_eq!(response.result.unwrap().actions, vec![action()]);
        assert_eq!(
            load(root.path(), project.path()).unwrap().actions,
            vec![action()]
        );
    }
}
