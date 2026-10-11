//! Explicit inspection of private, validated instruction copies in a journal.
//! These copies stay separate from the transcript and project document reads.

use openide_core::{
    system_instructions_digest, AgentEventKind, NativeReplay, StoredSession,
    SYSTEM_INSTRUCTIONS_VERSION,
};
use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeInstructionUiState {
    pub session_id: Option<String>,
    pub workspace: Option<PathBuf>,
    pub journal_last_sequence: u64,
    pub snapshots: Vec<NativeInstructionSnapshot>,
    pub selected_sequence: Option<u64>,
    pub selector_open: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeInstructionSnapshot {
    pub turn_id: Option<String>,
    pub turn_start_sequence: u64,
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub version: u16,
    pub digest: String,
    pub content: String,
}

impl NativeInstructionUiState {
    pub(crate) fn scoped_error(
        session_id: &str,
        workspace: PathBuf,
        error: impl Into<String>,
    ) -> Self {
        Self {
            session_id: Some(session_id.into()),
            workspace: Some(workspace.canonicalize().unwrap_or(workspace)),
            error: Some(error.into()),
            ..Self::default()
        }
    }

    /// The caller must replay this exact stored journal through the core's
    /// checkpoint-aware validation before projecting private instruction text.
    pub(crate) fn from_validated(
        stored: &StoredSession,
        replay: Result<NativeReplay, String>,
    ) -> Self {
        let mut state = Self {
            session_id: Some(stored.session.id.clone()),
            workspace: Some(
                stored
                    .session
                    .workspace
                    .canonicalize()
                    .unwrap_or_else(|_| stored.session.workspace.clone()),
            ),
            journal_last_sequence: stored.events.last().map_or(0, |event| event.sequence),
            ..Self::default()
        };
        if stored.session.backend != "native" {
            state.error = Some("La conversación guardada no usa el motor nativo".into());
            return state;
        }
        match replay {
            Err(error) => {
                state.error = Some(error);
                return state;
            }
            Ok(replay) if replay.last_sequence != state.journal_last_sequence => {
                state.error =
                    Some("La validación no corresponde al final del journal leído".into());
                return state;
            }
            Ok(_) => {}
        }
        // Check every private copy, including events preceding an authoritative
        // checkpoint. A matching last sequence alone does not authenticate text.
        for event in &stored.events {
            if event.session_id != stored.session.id {
                state.error = Some(format!(
                    "El evento {} pertenece a otra conversación",
                    event.sequence
                ));
                return state;
            }
            if let AgentEventKind::SystemInstructionsCommitted {
                version,
                digest,
                content,
            } = &event.kind
            {
                if *version != SYSTEM_INSTRUCTIONS_VERSION
                    || system_instructions_digest(content).as_deref() != Some(digest.as_str())
                {
                    state.error = Some(format!("La copia de instrucciones del evento {} tiene versión, tamaño o digest inválido", event.sequence));
                    return state;
                }
            }
        }
        // Idle replay clears active_system_instructions. Completed turns must
        // therefore be projected from the validated events, never that field.
        let mut turn_id = None;
        let mut turn_start_sequence = 0;
        for event in &stored.events {
            match &event.kind {
                AgentEventKind::TurnAdmitted { turn_id: id, .. }
                | AgentEventKind::TurnAdmittedWithImages { turn_id: id, .. } => {
                    turn_id = Some(id.clone());
                }
                AgentEventKind::TurnStarted { .. } => {
                    turn_start_sequence = event.sequence;
                }
                AgentEventKind::SystemInstructionsCommitted {
                    version,
                    digest,
                    content,
                } => state.snapshots.push(NativeInstructionSnapshot {
                    turn_id: turn_id.clone(),
                    turn_start_sequence,
                    sequence: event.sequence,
                    timestamp_ms: event.timestamp_ms,
                    version: *version,
                    digest: digest.clone(),
                    content: content.clone(),
                }),
                AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnFailed { .. }
                | AgentEventKind::TurnCancelled | AgentEventKind::TurnSuperseded { .. } => {
                    turn_id = None;
                    turn_start_sequence = 0;
                }
                _ => {}
            }
        }
        state.selected_sequence = state.snapshots.last().map(|snapshot| snapshot.sequence);
        state
    }

    pub(crate) fn selected_snapshot(&self) -> Option<&NativeInstructionSnapshot> {
        self.snapshots
            .iter()
            .find(|snapshot| Some(snapshot.sequence) == self.selected_sequence)
    }

    pub(crate) fn matches_scope(
        &self,
        session_id: Option<&str>,
        workspace: &std::path::Path,
    ) -> bool {
        self.session_id.as_deref() == session_id
            && self.session_id.is_some()
            && workspace.canonicalize().ok().as_deref() == self.workspace.as_deref()
    }
}

impl NativeInstructionSnapshot {
    pub(crate) fn identity_description(&self) -> String {
        let recorded_at = i64::try_from(self.timestamp_ms)
            .ok()
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%d/%m/%Y %H:%M:%S %:z")
                    .to_string()
            })
            .unwrap_or_else(|| "fecha fuera del rango representable".into());
        format!(
            "Turno: {}\nInicio del turno: evento {}\nCopia: evento {} · Unix ms {}\nRegistrada (hora local): {}\nVersión: {}\nSHA-256: {}\nTexto: {} bytes UTF-8\nÁmbito: instrucciones adicionales iniciales del turno; sin ID de llamada a herramienta",
            self.turn_id.as_deref().unwrap_or("legado sin ID de admisión"),
            self.turn_start_sequence,
            self.sequence,
            self.timestamp_ms,
            recorded_at,
            self.version,
            self.digest,
            self.content.len(),
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use openide_core::{
        replay_native, system_instructions_digest, AgentEvent, AgentMessage,
        CheckpointCompatibility, SessionStore, TurnMode, EXEC_POLICY_IDENTITY_VERSION,
        HOOK_CONFIG_IDENTITY_VERSION, MODEL_CONTEXT_VERSION, SYSTEM_INSTRUCTIONS_VERSION,
    };
    use std::path::Path;

    pub(crate) fn journal_fixture(
        data_root: &Path,
        workspace: &Path,
        contents: &[&str],
        complete: bool,
    ) -> (SessionStore, String) {
        let store = SessionStore::open(data_root.join("sessions")).unwrap();
        let session = store.create(workspace, "native").unwrap();
        for (index, content) in contents.iter().enumerate() {
            let turn_id = format!("instruction-turn-{index}");
            let prompt = format!("Fixture {index}");
            let admission = store
                .admit_native_turn(
                    &session.id,
                    &turn_id,
                    &prompt,
                    "fixture-model",
                    TurnMode::Build,
                )
                .unwrap();
            let mut kinds = vec![
                AgentEventKind::TurnStarted { prompt },
                AgentEventKind::ToolCatalogCommitted {
                    digest: "a".repeat(64),
                },
                AgentEventKind::HooksCommitted {
                    version: HOOK_CONFIG_IDENTITY_VERSION,
                    digest: "b".repeat(64),
                },
                AgentEventKind::ExecPolicyCommitted {
                    version: EXEC_POLICY_IDENTITY_VERSION,
                    digest: "c".repeat(64),
                },
                AgentEventKind::SystemInstructionsCommitted {
                    version: SYSTEM_INSTRUCTIONS_VERSION,
                    digest: system_instructions_digest(content).unwrap(),
                    content: (*content).into(),
                },
                AgentEventKind::ModelContextCommitted {
                    version: MODEL_CONTEXT_VERSION,
                    digest: "d".repeat(64),
                },
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "fixture-model".into(),
                },
            ];
            if complete {
                kinds.extend([
                    AgentEventKind::ModelResponseCommitted {
                        step: 1,
                        assistant: AgentMessage::assistant("Fixture completed", vec![]),
                        response_id: None,
                        usage: None,
                    },
                    AgentEventKind::TurnCompleted {
                        response: "Fixture completed".into(),
                        steps: 1,
                    },
                ]);
            }
            for (offset, kind) in kinds.into_iter().enumerate() {
                store
                    .append_native_event(&AgentEvent::new(
                        &session.id,
                        admission.sequence + offset as u64 + 1,
                        kind,
                    ))
                    .unwrap();
            }
        }
        (store, session.id)
    }

    pub(crate) fn validated_state(stored: &StoredSession) -> NativeInstructionUiState {
        let replay = replay_native(
            &stored.session.id,
            &stored.events,
            None,
            &CheckpointCompatibility {
                provider: String::new(),
                model: String::new(),
                context_format_key: String::new(),
            },
        )
        .map_err(|error| error.to_string());
        NativeInstructionUiState::from_validated(stored, replay)
    }

    #[test]
    fn instruction_inspection_retains_all_completed_turn_copies_after_reopen() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let original = "AGENTS.md inicial ñ\nSkill: respetar ámbito\n終わり";
        let changed = "AGENTS.md del siguiente turno\nSin sustituir la copia original";
        let (store, id) = journal_fixture(&data, root.path(), &[original, changed], true);
        let path = store.storage_root().join(format!("{id}.jsonl"));
        let before = std::fs::read(&path).unwrap();
        drop(store);
        std::fs::write(
            root.path().join("AGENTS.md"),
            "New disk document must not be read",
        )
        .unwrap();
        let reopened = SessionStore::open(data.join("sessions")).unwrap();
        let stored = reopened.load(&id).unwrap().unwrap();
        let state = validated_state(&stored);
        assert!(state.error.is_none());
        assert_eq!(state.snapshots.len(), 2);
        assert_eq!(state.snapshots[0].content, original);
        assert_eq!(state.snapshots[1].content, changed);
        assert_eq!(
            state.snapshots[0].turn_id.as_deref(),
            Some("instruction-turn-0")
        );
        assert_eq!(
            state.snapshots[1].turn_id.as_deref(),
            Some("instruction-turn-1")
        );
        assert_eq!(
            state.snapshots[0].digest,
            system_instructions_digest(original).unwrap()
        );
        assert_eq!(state.snapshots[0].version, SYSTEM_INSTRUCTIONS_VERSION);
        assert!(state.snapshots[1].sequence > state.snapshots[0].sequence);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn instruction_inspection_rejects_unvalidated_version_digest_order_and_identity() {
        let root = tempfile::tempdir().unwrap();
        let (store, id) = journal_fixture(root.path(), root.path(), &["Private exact text"], false);
        let stored = store.load(&id).unwrap().unwrap();
        let valid = validated_state(&stored);
        assert!(valid.error.is_none());
        assert_eq!(valid.snapshots.len(), 1);
        for mutation in 0..4 {
            let mut invalid = stored.clone();
            let event = invalid
                .events
                .iter_mut()
                .find(|event| {
                    matches!(
                        event.kind,
                        AgentEventKind::SystemInstructionsCommitted { .. }
                    )
                })
                .unwrap();
            match mutation {
                0 => {
                    if let AgentEventKind::SystemInstructionsCommitted { version, .. } =
                        &mut event.kind
                    {
                        *version += 1;
                    }
                }
                1 => {
                    if let AgentEventKind::SystemInstructionsCommitted { digest, .. } =
                        &mut event.kind
                    {
                        *digest = "0".repeat(64);
                    }
                }
                2 => event.session_id = "foreign-session".into(),
                _ => event.sequence += 1,
            }
            let rejected = validated_state(&invalid);
            assert!(rejected.error.is_some());
            assert!(
                rejected.snapshots.is_empty(),
                "private text projected before validation"
            );
        }
        let failed = NativeInstructionUiState::from_validated(
            &stored,
            Err("Checkpoint could not be verified".into()),
        );
        assert_eq!(
            failed.error.as_deref(),
            Some("Checkpoint could not be verified")
        );
        assert!(failed.snapshots.is_empty());
    }
}
