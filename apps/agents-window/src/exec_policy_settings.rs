//! Host-authored argv prefix rules for native command tools.
//!
//! The file lives under the application's data root, outside the agent's
//! workspace. Loading is bounded and fail closed; only a validated snapshot
//! is handed to the harness at the start of a native turn.

use crate::agent_tool_settings::replace_atomically;
use openide_harness::{ExecDecision, ExecEvaluation, ExecPatternToken, ExecPolicy, ExecRule};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const FILE_NAME: &str = "native-exec-rules.json";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 64 * 1024;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default)]
pub(crate) struct ExecPolicySettings {
    pub rules: Vec<ExecRule>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecPolicyUiState {
    pub rules: Vec<ExecRule>,
    pub load_error: Option<String>,
    pub editor_open: bool,
    pub reset_confirm_open: bool,
    pub selected_index: Option<usize>,
    pub draft_pattern: String,
    pub draft_decision: ExecDecision,
    pub draft_justification: String,
    pub preview_argv: String,
    pub preview: Option<Result<ExecPolicyPreview, String>>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecPolicyPreview {
    pub argv: Vec<String>,
    pub policy_digest: String,
    pub evaluation: ExecEvaluation,
    pub matched_rules: Vec<(usize, ExecRule)>,
}

impl ExecPolicyPreview {
    pub fn description(&self) -> String {
        let decision = match self.evaluation.decision {
            Some(ExecDecision::Ask) => "Pedir aprobación",
            Some(ExecDecision::Deny) => "Bloquear",
            Some(ExecDecision::Allow) => "Permitir según reglas",
            None => "Sin coincidencias",
        };
        let mut description = format!(
            "Resultado: {decision}\nArgumentos exactos: {}\nIdentidad de las reglas guardadas: {}\n",
            serde_json::to_string_pretty(&self.argv).expect("string argv is serializable"),
            self.policy_digest
        );
        if self.evaluation.opaque_wrapper {
            description.push_str("El ejecutable envuelve un script. El motor aplica la restricción conservadora para intérpretes; no analiza el contenido del script.\n");
        }
        if self.matched_rules.is_empty() {
            description.push_str("Ningún prefijo guardado coincide con estos argumentos.\n");
        }
        for (index, rule) in &self.matched_rules {
            let decision = match rule.decision() {
                ExecDecision::Ask => "Pedir aprobación",
                ExecDecision::Deny => "Bloquear",
                ExecDecision::Allow => "Permitir según reglas",
            };
            description.push_str(&format!(
                "\nRegla {} · {decision}\nPrefijo: {}\n",
                index + 1,
                serde_json::to_string_pretty(rule.pattern())
                    .expect("validated rule is serializable")
            ));
            if let Some(reason) = rule.justification() {
                description.push_str(&format!("Motivo: {reason}\n"));
            }
        }
        description.push_str("\nEsta vista evalúa las reglas guardadas sin ejecutar el comando. Los permisos de herramientas y de sesión se comprueban al ejecutar.");
        description
    }
}

impl Default for ExecPolicyUiState {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            load_error: None,
            editor_open: false,
            reset_confirm_open: false,
            selected_index: None,
            draft_pattern: "[]".into(),
            draft_decision: ExecDecision::Ask,
            draft_justification: String::new(),
            preview_argv: "[]".into(),
            preview: None,
        }
    }
}

impl ExecPolicyUiState {
    pub fn from_settings(settings: &ExecPolicySettings) -> Self {
        Self {
            rules: settings.rules.clone(),
            ..Self::default()
        }
    }

    pub fn clear_draft(&mut self) {
        self.editor_open = false;
        self.reset_confirm_open = false;
        self.selected_index = None;
        self.draft_pattern = "[]".into();
        self.draft_decision = ExecDecision::Ask;
        self.draft_justification.clear();
        self.preview = None;
    }

    pub fn set_preview_argv(&mut self, value: String) {
        self.preview_argv = value;
        self.preview = None;
    }

    /// Inspect a freshly read saved policy. The draft and effective runtime
    /// settings are untouched; this snapshot is never an execution grant.
    pub fn preview_saved(&mut self, settings: Result<ExecPolicySettings, String>) {
        self.preview = Some((|| {
            let settings = settings?;
            if self.preview_argv.len() as u64 > MAX_BYTES {
                return Err("Los argumentos de la vista previa exceden el límite de tamaño".into());
            }
            let argv: Vec<String> = serde_json::from_str(&self.preview_argv).map_err(|error| {
                format!("Los argumentos deben ser un arreglo JSON de cadenas: {error}")
            })?;
            let policy = settings.policy()?;
            let evaluation = policy
                .evaluate_argv(&argv)
                .map_err(|error| format!("No se pudo evaluar el comando: {error}"))?;
            let matched_rules = evaluation
                .matched_rule_indices
                .iter()
                .map(|index| (*index, settings.rules[*index].clone()))
                .collect();
            Ok(ExecPolicyPreview {
                argv,
                policy_digest: policy.digest_hex(),
                evaluation,
                matched_rules,
            })
        })());
    }

    pub fn select(&mut self, index: usize) -> Result<(), String> {
        let rule = self
            .rules
            .get(index)
            .ok_or_else(|| "La regla seleccionada ya no existe".to_owned())?;
        self.draft_pattern = serde_json::to_string(rule.pattern())
            .map_err(|error| format!("No se pudo mostrar el prefijo: {error}"))?;
        self.draft_decision = rule.decision();
        self.draft_justification = rule.justification().unwrap_or_default().to_owned();
        self.selected_index = Some(index);
        self.editor_open = true;
        Ok(())
    }

    pub fn build_draft_rule(&self) -> Result<ExecRule, String> {
        if self.load_error.is_some() {
            return Err("Corregí el archivo de reglas antes de editarlo".into());
        }
        if self.draft_decision == ExecDecision::Allow {
            return Err("La interfaz sólo permite solicitar aprobación o bloquear".into());
        }
        if self.draft_pattern.len() as u64 > MAX_BYTES {
            return Err("El prefijo excede el límite de tamaño".into());
        }
        let pattern: Vec<ExecPatternToken> = serde_json::from_str(&self.draft_pattern)
            .map_err(|error| format!("El prefijo debe ser un arreglo JSON de tokens: {error}"))?;
        let justification = match self.draft_justification.trim() {
            "" => None,
            value => Some(value.to_owned()),
        };
        ExecRule::new(pattern, self.draft_decision, justification)
            .map_err(|error| format!("Regla de comando no válida: {error}"))
    }

    pub fn upsert_draft(&mut self) -> Result<(), String> {
        let rule = self.build_draft_rule()?;
        let mut next = self.rules.clone();
        match self.selected_index {
            Some(index) => {
                let existing = next
                    .get_mut(index)
                    .ok_or_else(|| "La regla seleccionada ya no existe".to_owned())?;
                *existing = rule;
            }
            None => next.push(rule),
        }
        ExecPolicySettings {
            rules: next.clone(),
        }
        .policy()?;
        self.rules = next;
        self.preview = None;
        self.clear_draft();
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> Result<(), String> {
        if self.load_error.is_some() {
            return Err("Corregí el archivo de reglas antes de editarlo".into());
        }
        if index >= self.rules.len() {
            return Err("La regla seleccionada ya no existe".into());
        }
        self.rules.remove(index);
        self.preview = None;
        self.clear_draft();
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    rules: Vec<ExecRule>,
}

impl ExecPolicySettings {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(FILE_NAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(format!("No se pudo inspeccionar las reglas: {error}")),
        };
        if !metadata.is_file() || is_link_or_reparse(&metadata) || metadata.len() > MAX_BYTES {
            return Err("El archivo de reglas no es válido o excede el límite de tamaño".into());
        }
        let mut file = open_no_follow(&path)
            .map_err(|error| format!("No se pudo abrir las reglas de comandos: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("No se pudo inspeccionar las reglas: {error}"))?;
        if !metadata.is_file() || is_link_or_reparse(&metadata) || metadata.len() > MAX_BYTES {
            return Err("El archivo de reglas no es válido o excede el límite de tamaño".into());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("No se pudo leer las reglas de comandos: {error}"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("El archivo de reglas excede el límite de tamaño".into());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Las reglas de comandos no son válidas: {error}"))?;
        if document.version != VERSION {
            return Err(format!(
                "Versión de reglas de comandos no compatible: {}",
                document.version
            ));
        }
        let settings = Self {
            rules: document.rules,
        };
        settings.policy()?;
        Ok(settings)
    }

    pub fn policy(&self) -> Result<ExecPolicy, String> {
        if self
            .rules
            .iter()
            .any(|rule| rule.decision() == ExecDecision::Allow)
        {
            return Err(
                "Las reglas de Settings sólo admiten solicitar aprobación o bloquear".into(),
            );
        }
        ExecPolicy::new(self.rules.clone())
            .map_err(|error| format!("Reglas de comandos no válidas: {error}"))
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        self.policy()?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            rules: self.rules.clone(),
        })
        .map_err(|error| format!("No se pudo codificar las reglas: {error}"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Las reglas de comandos exceden el límite de tamaño".into());
        }
        fs::create_dir_all(root)
            .map_err(|error| format!("No se pudo crear la carpeta de configuración: {error}"))?;
        let temporary = root.join(format!(
            ".native-exec-rules-{}-{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            replace_atomically(&temporary, &root.join(FILE_NAME))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|error| format!("No se pudo guardar las reglas de comandos: {error}"))
    }
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> ExecRule {
        ExecRule::new(
            vec![
                ExecPatternToken::Literal("git".into()),
                ExecPatternToken::AnyOf(vec!["push".into(), "reset".into()]),
            ],
            ExecDecision::Deny,
            Some("Operación destructiva".into()),
        )
        .unwrap()
    }

    #[test]
    fn settings_round_trip_preserves_policy_digest() {
        let root = tempfile::tempdir().unwrap();
        let settings = ExecPolicySettings {
            rules: vec![rule()],
        };
        settings.save(root.path()).unwrap();
        let loaded = ExecPolicySettings::load(root.path()).unwrap();
        assert_eq!(
            settings.policy().unwrap().digest_hex(),
            loaded.policy().unwrap().digest_hex()
        );
    }

    #[test]
    fn preview_uses_saved_rules_after_reopen_and_preserves_files_and_policy() {
        let root = tempfile::tempdir().unwrap();
        let settings = ExecPolicySettings {
            rules: vec![
                ExecRule::new(
                    vec![ExecPatternToken::Literal("git".into())],
                    ExecDecision::Ask,
                    Some("Revisar argumentos".into()),
                )
                .unwrap(),
                rule(),
            ],
        };
        settings.save(root.path()).unwrap();
        let saved_bytes = fs::read(root.path().join(FILE_NAME)).unwrap();
        let loaded = ExecPolicySettings::load(root.path()).unwrap();
        let digest = loaded.policy().unwrap().digest_hex();
        let mut ui = ExecPolicyUiState::from_settings(&loaded);
        ui.draft_pattern = "[\"inventado\"]".into();
        ui.draft_decision = ExecDecision::Deny;
        for (argv, decision, matched, opaque) in [
            (
                r#"["git","push","origen","rama"]"#,
                Some(ExecDecision::Deny),
                vec![0, 1],
                false,
            ),
            (
                r#"["git","status"]"#,
                Some(ExecDecision::Ask),
                vec![0],
                false,
            ),
            (r#"["otro","ámbito\ncompleto"]"#, None, vec![], false),
            (
                r#"["sh","-c","printf prueba"]"#,
                Some(ExecDecision::Deny),
                vec![],
                true,
            ),
        ] {
            ui.set_preview_argv(argv.into());
            ui.preview_saved(ExecPolicySettings::load(root.path()));
            let preview = ui.preview.as_ref().unwrap().as_ref().unwrap();
            assert_eq!(preview.evaluation.decision, decision);
            assert_eq!(preview.evaluation.matched_rule_indices, matched);
            assert_eq!(preview.evaluation.opaque_wrapper, opaque);
            assert_eq!(preview.policy_digest, digest);
            assert_eq!(
                preview.argv,
                serde_json::from_str::<Vec<String>>(argv).unwrap()
            );
            assert_eq!(fs::read(root.path().join(FILE_NAME)).unwrap(), saved_bytes);
            assert_eq!(ui.rules, loaded.rules);
            assert_eq!(ui.draft_pattern, "[\"inventado\"]");
        }
        assert!(ui
            .preview
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .description()
            .contains("sin ejecutar"));
        fs::write(root.path().join(FILE_NAME), b"{corrupto").unwrap();
        ui.preview_saved(ExecPolicySettings::load(root.path()));
        assert!(ui.preview.as_ref().unwrap().is_err());
        assert_eq!(fs::read(root.path().join(FILE_NAME)).unwrap(), b"{corrupto");
        assert_eq!(ui.rules, loaded.rules);
        settings.save(root.path()).unwrap();
        ui.preview_saved(ExecPolicySettings::load(root.path()));
        assert_eq!(
            ui.preview.as_ref().unwrap().as_ref().unwrap().policy_digest,
            digest
        );
    }

    #[test]
    fn preview_rejects_invalid_arguments_without_a_stale_report() {
        let settings = ExecPolicySettings {
            rules: vec![rule()],
        };
        let mut ui = ExecPolicyUiState::from_settings(&settings);
        ui.set_preview_argv("[\"git\",\"push\"]".into());
        ui.preview_saved(Ok(settings.clone()));
        assert!(ui.preview.as_ref().unwrap().is_ok());
        for input in [
            "[]",
            "[null]",
            "{\"command\":\"git\"}",
            "[\"\"]",
            "[\"git\\u0000\"]",
        ] {
            ui.set_preview_argv(input.into());
            assert!(ui.preview.is_none());
            ui.preview_saved(Ok(settings.clone()));
            assert!(ui.preview.as_ref().unwrap().is_err(), "{input}");
        }
        ui.set_preview_argv("x".repeat(MAX_BYTES as usize + 1));
        ui.preview_saved(Ok(settings));
        assert!(ui
            .preview
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap_err()
            .contains("tamaño"));
    }

    #[test]
    fn corrupt_oversized_unknown_and_allow_files_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(FILE_NAME);
        fs::write(&path, b"{broken").unwrap();
        assert!(ExecPolicySettings::load(root.path()).is_err());
        fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert!(ExecPolicySettings::load(root.path()).is_err());
        fs::write(&path, br#"{"version":2,"rules":[]}"#).unwrap();
        assert!(ExecPolicySettings::load(root.path()).is_err());
        fs::write(
            &path,
            br#"{"version":1,"rules":[{"pattern":["git"],"decision":"deny","justification":null,"unexpected":"ignored"}]}"#,
        )
        .unwrap();
        assert!(ExecPolicySettings::load(root.path()).is_err());
        let allow = ExecRule::new(
            vec![ExecPatternToken::Literal("git".into())],
            ExecDecision::Allow,
            None,
        )
        .unwrap();
        let document = Document {
            version: VERSION,
            rules: vec![allow],
        };
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(ExecPolicySettings::load(root.path()).is_err());
    }

    #[test]
    fn editor_accepts_union_and_rejects_malformed_or_stale_rule() {
        let mut state = ExecPolicyUiState {
            draft_pattern: r#"["git",["push","reset"]]"#.into(),
            draft_decision: ExecDecision::Deny,
            ..ExecPolicyUiState::default()
        };
        state.upsert_draft().unwrap();
        assert_eq!(state.rules, vec![rule_without_justification()]);
        state.select(0).unwrap();
        assert_eq!(state.draft_pattern, r#"["git",["push","reset"]]"#);
        state.draft_pattern = "[\"git\",[]]".into();
        assert!(state.upsert_draft().is_err());
        state.draft_pattern = "[\"git\"]".into();
        state.draft_decision = ExecDecision::Ask;
        state.upsert_draft().unwrap();
        assert_eq!(state.rules[0].decision(), ExecDecision::Ask);
        assert!(state.remove(1).is_err());
    }

    fn rule_without_justification() -> ExecRule {
        ExecRule::new(
            vec![
                ExecPatternToken::Literal("git".into()),
                ExecPatternToken::AnyOf(vec!["push".into(), "reset".into()]),
            ],
            ExecDecision::Deny,
            None,
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_file_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(FILE_NAME)).unwrap();
        assert!(ExecPolicySettings::load(root.path()).is_err());
    }
}
