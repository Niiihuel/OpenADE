//! Rebuildable, read-only projection of a native automation journal.
//! The session origin and strict replay are authority; the scheduler ledger is
//! a bounded view. Derivation never reads credentials or current Settings.

use crate::native_automation::FrozenNativeAutomationAuthorityV1;
use openide_core::{
    native_automation_first_turn_id, native_automation_session_id, replay_native,
    system_instructions_digest, AgentEvent, AgentEventKind, CheckpointFileStore, ReplayRecovery,
    StoredSession, TurnMode, EXEC_POLICY_IDENTITY_VERSION, HOOK_CONFIG_IDENTITY_VERSION,
    MODEL_CONTEXT_VERSION, MODEL_ROUTE_VERSION, SYSTEM_INSTRUCTIONS_VERSION,
};
use openide_harness::CommandHooks;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAutomationOutcome {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAutomationResultV1 {
    version: u16,
    session_id: String,
    workspace: PathBuf,
    automation_id: String,
    run_id: String,
    scheduled_at_ms: i64,
    manual: bool,
    authority_digest: String,
    first_turn_id: String,
    outcome: NativeAutomationOutcome,
    recovery_required: bool,
    last_sequence: u64,
    journal_digest: String,
    terminal_sequence: Option<u64>,
    terminal_digest: Option<String>,
}

/// Proof produced only by full semantic derivation. Persisted DTOs can be
/// inspected after Deserialize, but cannot become a new ledger observation.
#[derive(Debug)]
pub struct ValidatedNativeAutomationResult {
    result: NativeAutomationResultV1,
}

impl std::ops::Deref for ValidatedNativeAutomationResult {
    type Target = NativeAutomationResultV1;
    fn deref(&self) -> &Self::Target {
        &self.result
    }
}

impl ValidatedNativeAutomationResult {
    pub(crate) fn snapshot(&self) -> &NativeAutomationResultV1 {
        &self.result
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn event_digest(event: &AgentEvent) -> Result<String, String> {
    serde_json::to_vec(event)
        .map(|bytes| hex_digest(&bytes))
        .map_err(|error| error.to_string())
}

/// Length-delimited canonical event serialization, independent of whitespace
/// in the JSONL file. Prefix comparison detects changed or truncated history.
fn journal_digest(events: &[AgentEvent], through: u64) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"openide-native-automation-result-journal-v1\0");
    let mut count = 0_u64;
    for event in events.iter().take_while(|event| event.sequence <= through) {
        count = count.checked_add(1).ok_or("Journal demasiado grande")?;
        if event.sequence != count {
            return Err("Secuencia inválida en el resultado de automatización".into());
        }
        let bytes = serde_json::to_vec(event).map_err(|error| error.to_string())?;
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    if count != through {
        return Err("El journal no conserva el prefijo observado".into());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl NativeAutomationResultV1 {
    /// The host must hold the native lease while loading this record and
    /// publishing its projection. A live worker is never called Interrupted.
    pub(crate) fn derive(
        record: &StoredSession,
        checkpoints: &CheckpointFileStore,
    ) -> Result<ValidatedNativeAutomationResult, String> {
        if record.events.is_empty() {
            return Err("La automatización registrada aún no tiene una admisión".into());
        }
        let frozen = FrozenNativeAutomationAuthorityV1::from_session(&record.session)?
            .ok_or("La sesión no es una automatización nativa registrada")?;
        let origin = record
            .session
            .native_automation
            .as_ref()
            .expect("validated origin");
        let replay = replay_native(
            &record.session.id,
            &record.events,
            Some(checkpoints),
            &frozen.compatibility(),
        )
        .map_err(|error| format!("Resultado de automatización no verificable: {error}"))?;
        validate_original_profile(record, &frozen)?;
        let terminal = record
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    AgentEventKind::TurnCompleted { .. }
                        | AgentEventKind::TurnFailed { .. }
                        | AgentEventKind::TurnCancelled | AgentEventKind::TurnSuperseded { .. }
                )
            })
            .collect::<Vec<_>>();
        if terminal.len() > 1 {
            return Err("La automatización tiene más de un resultado terminal".into());
        }
        let terminal = terminal.first().copied();
        let outcome = match terminal.map(|event| &event.kind) {
            Some(AgentEventKind::TurnCompleted { .. }) => NativeAutomationOutcome::Completed,
            Some(AgentEventKind::TurnFailed { .. }) => NativeAutomationOutcome::Failed,
            Some(AgentEventKind::TurnCancelled) => NativeAutomationOutcome::Cancelled,
            _ => NativeAutomationOutcome::Interrupted,
        };
        let recovery_required = outcome == NativeAutomationOutcome::Interrupted
            || replay.recovery != ReplayRecovery::Idle;
        if outcome == NativeAutomationOutcome::Completed && recovery_required {
            return Err("La automatización completada conserva una recuperación pendiente".into());
        }
        let result = Self {
            version: 1,
            session_id: record.session.id.clone(),
            workspace: record.session.workspace.clone(),
            automation_id: origin.automation_id.clone(),
            run_id: origin.run_id.clone(),
            scheduled_at_ms: origin.scheduled_at_ms,
            manual: origin.manual,
            authority_digest: origin.authority_snapshot_digest.clone(),
            first_turn_id: origin.first_turn_id.clone(),
            outcome,
            recovery_required,
            last_sequence: replay.last_sequence,
            journal_digest: journal_digest(&record.events, replay.last_sequence)?,
            terminal_sequence: terminal.map(|event| event.sequence),
            terminal_digest: terminal.map(event_digest).transpose()?,
        };
        result.verify_record(record)?;
        Ok(ValidatedNativeAutomationResult { result })
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || !self.workspace.is_absolute()
            || native_automation_session_id(&self.workspace, &self.run_id)
                .map_err(|error| error.to_string())?
                != self.session_id
            || native_automation_first_turn_id(&self.run_id).map_err(|error| error.to_string())?
                != self.first_turn_id
            || !valid_digest(&self.authority_digest)
            || !valid_digest(&self.journal_digest)
            || self.automation_id.is_empty()
            || self.automation_id.len() > 64
            || self.scheduled_at_ms < 0
            || self.last_sequence == 0
        {
            return Err("Resultado de automatización inválido".into());
        }
        match (self.outcome, self.terminal_sequence, &self.terminal_digest) {
            (NativeAutomationOutcome::Interrupted, None, None) if self.recovery_required => {}
            (NativeAutomationOutcome::Completed, Some(sequence), Some(digest))
                if !self.recovery_required
                    && sequence > 0
                    && sequence <= self.last_sequence
                    && valid_digest(digest) => {}
            (
                NativeAutomationOutcome::Failed | NativeAutomationOutcome::Cancelled,
                Some(sequence),
                Some(digest),
            ) if sequence > 0 && sequence <= self.last_sequence && valid_digest(digest) => {}
            _ => return Err("Resultado terminal de automatización inconsistente".into()),
        }
        Ok(())
    }

    fn verify_origin(&self, record: &StoredSession) -> Result<(), String> {
        self.validate()?;
        FrozenNativeAutomationAuthorityV1::from_session(&record.session)?
            .ok_or("La sesión no tiene una autoridad nativa verificable")?;
        let origin = record
            .session
            .native_automation
            .as_ref()
            .expect("validated origin");
        if record.session.id != self.session_id
            || record.session.workspace != self.workspace
            || origin.automation_id != self.automation_id
            || origin.run_id != self.run_id
            || origin.scheduled_at_ms != self.scheduled_at_ms
            || origin.manual != self.manual
            || origin.authority_snapshot_digest != self.authority_digest
            || origin.first_turn_id != self.first_turn_id
        {
            return Err("El resultado pertenece a otra automatización o autoridad".into());
        }
        Ok(())
    }

    pub(crate) fn verify_record_prefix(&self, record: &StoredSession) -> Result<(), String> {
        self.verify_origin(record)?;
        if journal_digest(&record.events, self.last_sequence)? != self.journal_digest {
            return Err("El journal cambió el prefijo del resultado registrado".into());
        }
        if let Some(sequence) = self.terminal_sequence {
            let event = record
                .events
                .iter()
                .find(|event| event.sequence == sequence)
                .ok_or("Falta el evento terminal registrado")?;
            if self.terminal_digest.as_deref() != Some(event_digest(event)?.as_str())
                || !matches!(
                    (&event.kind, self.outcome),
                    (
                        AgentEventKind::TurnCompleted { .. },
                        NativeAutomationOutcome::Completed
                    ) | (
                        AgentEventKind::TurnFailed { .. },
                        NativeAutomationOutcome::Failed
                    ) | (
                        AgentEventKind::TurnCancelled,
                        NativeAutomationOutcome::Cancelled
                    )
                )
            {
                return Err("El journal cambió el resultado terminal registrado".into());
            }
        }
        Ok(())
    }

    pub(crate) fn verify_record(&self, record: &StoredSession) -> Result<(), String> {
        self.verify_record_prefix(record)?;
        if record.events.last().map_or(0, |event| event.sequence) != self.last_sequence {
            return Err("El resultado no corresponde al journal completo observado".into());
        }
        let terminals = record
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    AgentEventKind::TurnCompleted { .. }
                        | AgentEventKind::TurnFailed { .. }
                        | AgentEventKind::TurnCancelled | AgentEventKind::TurnSuperseded { .. }
                )
            })
            .collect::<Vec<_>>();
        if terminals.len() > 1
            || terminals.first().map(|event| event.sequence) != self.terminal_sequence
        {
            return Err("El resultado no corresponde al único terminal del journal".into());
        }
        Ok(())
    }

    pub(crate) fn outcome(&self) -> NativeAutomationOutcome {
        self.outcome
    }
    pub(crate) fn recovery_required(&self) -> bool {
        self.recovery_required
    }
    pub(crate) fn last_sequence(&self) -> u64 {
        self.last_sequence
    }
    pub(crate) fn terminal_sequence(&self) -> Option<u64> {
        self.terminal_sequence
    }
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }
    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }
    pub(crate) fn automation_id(&self) -> &str {
        &self.automation_id
    }
    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }
    pub(crate) fn scheduled_at_ms(&self) -> i64 {
        self.scheduled_at_ms
    }
    pub(crate) fn manual(&self) -> bool {
        self.manual
    }
}

fn validate_original_profile(
    record: &StoredSession,
    frozen: &FrozenNativeAutomationAuthorityV1,
) -> Result<(), String> {
    let route = frozen.route_snapshot()?;
    let catalog = frozen.catalog()?.digest_hex();
    let policy = frozen.policy().digest_hex();
    let hooks = CommandHooks::default()
        .digest_hex()
        .map_err(|error| error.to_string())?;
    let instructions = system_instructions_digest(frozen.instructions())
        .ok_or("Instrucciones de automatización inválidas")?;
    let context = frozen.recorded_context_digest()?;
    let mut admitted = false;
    let mut committed_catalog = false;
    let mut committed_policy = false;
    let mut committed_hooks = false;
    let mut committed_instructions = false;
    let mut committed_context = false;
    let mut committed_route = false;
    let mut requested_model = false;
    for event in &record.events {
        let valid = match &event.kind {
            AgentEventKind::SessionStarted { workspace } => workspace == &record.session.workspace,
            AgentEventKind::TurnAdmitted {
                turn_id,
                prompt,
                model,
                mode,
            } => {
                let valid = !admitted
                    && turn_id == frozen.first_turn_id()
                    && prompt == frozen.prompt()
                    && model == frozen.model_id()
                    && *mode == TurnMode::Build;
                admitted = true;
                valid
            }
            AgentEventKind::TurnStarted { prompt } => admitted && prompt == frozen.prompt(),
            AgentEventKind::ToolCatalogCommitted { digest } => {
                committed_catalog = true;
                *digest == catalog
            }
            AgentEventKind::ExecPolicyCommitted { version, digest } => {
                committed_policy = true;
                *version == EXEC_POLICY_IDENTITY_VERSION && *digest == policy
            }
            AgentEventKind::HooksCommitted { version, digest } => {
                committed_hooks = true;
                *version == HOOK_CONFIG_IDENTITY_VERSION && *digest == hooks
            }
            AgentEventKind::SystemInstructionsCommitted {
                version,
                digest,
                content,
            } => {
                committed_instructions = true;
                *version == SYSTEM_INSTRUCTIONS_VERSION
                    && *digest == instructions
                    && content == frozen.instructions()
            }
            AgentEventKind::ModelContextCommitted { version, digest } => {
                committed_context = true;
                *version == MODEL_CONTEXT_VERSION && *digest == context
            }
            AgentEventKind::ModelRouteCommitted {
                version,
                provider_id,
                model_id,
                endpoint_digest,
                request_profile_digest,
            } => {
                committed_route = true;
                *version == MODEL_ROUTE_VERSION
                    && *provider_id == route.provider_id
                    && *model_id == route.model_id
                    && *endpoint_digest == route.endpoint_digest
                    && *request_profile_digest == route.request_profile_digest
            }
            AgentEventKind::ModelRequestStarted { model, .. } => {
                requested_model = true;
                model == frozen.model_id()
                    && committed_catalog
                    && committed_policy
                    && committed_hooks
                    && committed_instructions
                    && committed_context
                    && committed_route
            }
            AgentEventKind::TurnCompleted { .. } => requested_model,
            AgentEventKind::ToolStarted { tool_call } => matches!(
                tool_call.name.as_str(),
                "read_file" | "list_files" | "search_files" | "update_plan"
            ),
            AgentEventKind::TurnAdmittedWithImages { .. }
            | AgentEventKind::TurnSteered { .. }
            | AgentEventKind::TurnSteeredWithImages { .. }
            | AgentEventKind::McpRuntimeCommitted { .. }
            | AgentEventKind::HookStarted { .. }
            | AgentEventKind::HookFinished { .. } => false,
            _ => true,
        };
        if !valid {
            return Err(format!(
                "El journal no conserva la autoridad original en {}",
                event.sequence
            ));
        }
    }
    if !admitted {
        return Err("Falta la admisión original de la automatización".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_provider::{NativeWireApi, RuntimeConnection};
    use openide_core::{AgentMessage, SessionStore, ToolCall};
    use openide_harness::{record_verified_tool_outcome, ToolOutput};

    fn pending_fixture(
        root: &Path,
        cancelled: bool,
    ) -> (SessionStore, String, CheckpointFileStore) {
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let automation_id = uuid::Uuid::new_v4().to_string();
        let run_id = format!("{automation_id}:manual:{}", uuid::Uuid::new_v4());
        let frozen = FrozenNativeAutomationAuthorityV1::freeze(
            RuntimeConnection {
                auth: Default::default(),
                headers: Default::default(),
                endpoint: "http://127.0.0.1:9/v1".into(),
                api_key: None,
                model_id: "result-fixture/exact".into(),
                wire_api: NativeWireApi::ChatCompletions,
            },
            "custom",
            root,
            &workspace,
            None,
            &[],
            None,
            None,
            "Original frozen instructions".into(),
            "Read fixture".into(),
            &run_id,
            false,
        )
        .unwrap();
        let store = SessionStore::open(root.join("sessions")).unwrap();
        let session = store
            .create_native_automation(
                &workspace,
                frozen
                    .to_origin(&automation_id, &run_id, 1_500_000, true)
                    .unwrap(),
            )
            .unwrap();
        store
            .admit_native_turn(
                &session.id,
                frozen.first_turn_id(),
                frozen.prompt(),
                frozen.model_id(),
                TurnMode::Build,
            )
            .unwrap();
        let route = frozen.route_snapshot().unwrap();
        let call = ToolCall {
            id: "read-result-fixture".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path":"receipt.txt"}),
        };
        // A frozen journal prefix fixture; the recovery API below performs the
        // real human verification commit. No model or read effect is claimed.
        let kinds = vec![
            AgentEventKind::TurnStarted {
                prompt: frozen.prompt().into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: frozen.catalog().unwrap().digest_hex(),
            },
            AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
            AgentEventKind::ExecPolicyCommitted {
                version: EXEC_POLICY_IDENTITY_VERSION,
                digest: frozen.policy().digest_hex(),
            },
            AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: system_instructions_digest(frozen.instructions()).unwrap(),
                content: frozen.instructions().into(),
            },
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: frozen.recorded_context_digest().unwrap(),
            },
            AgentEventKind::ModelRouteCommitted {
                version: MODEL_ROUTE_VERSION,
                provider_id: route.provider_id,
                model_id: route.model_id,
                endpoint_digest: route.endpoint_digest,
                request_profile_digest: route.request_profile_digest,
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: frozen.model_id().into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("Read fixture", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted { tool_call: call },
            if cancelled {
                AgentEventKind::TurnCancelled
            } else {
                AgentEventKind::TurnFailed {
                    error: "Fixture failure with uncertain reading".into(),
                }
            },
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            store
                .append_native_event(&AgentEvent::new(&session.id, index as u64 + 2, kind))
                .unwrap();
        }
        let checkpoints = CheckpointFileStore::new(root.join("sessions")).unwrap();
        (store, session.id, checkpoints)
    }

    #[test]
    fn verified_pending_read_closes_recovery_without_replacing_failure_or_cancellation() {
        for cancelled in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (store, id, checkpoints) = pending_fixture(root.path(), cancelled);
            let record = store.load_strict(&id).unwrap().unwrap();
            let before = NativeAutomationResultV1::derive(&record, &checkpoints).unwrap();
            assert!(before.recovery_required());
            assert_eq!(
                before.outcome(),
                if cancelled {
                    NativeAutomationOutcome::Cancelled
                } else {
                    NativeAutomationOutcome::Failed
                }
            );
            let terminal = before.terminal_sequence();
            let prefix = record.events.clone();
            let lease = store.try_acquire_native_lease(&id).unwrap();
            let step = record_verified_tool_outcome(
                &store,
                &checkpoints,
                &lease,
                "read-result-fixture",
                ToolOutput::success("Explicitly verified original receipt"),
                16_000,
            )
            .unwrap();
            assert_eq!(step.replay.recovery, ReplayRecovery::Idle);
            drop(lease);
            let reopened = SessionStore::open(root.path().join("sessions")).unwrap();
            let record = reopened.load_strict(&id).unwrap().unwrap();
            let after = NativeAutomationResultV1::derive(&record, &checkpoints).unwrap();
            before.verify_record_prefix(&record).unwrap();
            assert_eq!(after.outcome(), before.outcome());
            assert_eq!(after.terminal_sequence(), terminal);
            assert!(!after.recovery_required());
            assert_eq!(record.events[..prefix.len()], prefix);
            assert_eq!(record.events.len(), prefix.len() + 1);
            assert!(!record
                .events
                .iter()
                .any(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. })));
        }
    }

    #[test]
    fn semantic_derivation_rejects_mixed_original_route_context_and_duplicate_terminals() {
        let root = tempfile::tempdir().unwrap();
        let (store, id, checkpoints) = pending_fixture(root.path(), false);
        let record = store.load_strict(&id).unwrap().unwrap();
        let original = NativeAutomationResultV1::derive(&record, &checkpoints).unwrap();
        for context in [false, true] {
            let mut mixed = record.clone();
            for event in &mut mixed.events {
                match &mut event.kind {
                    AgentEventKind::ModelRouteCommitted {
                        endpoint_digest, ..
                    } if !context => *endpoint_digest = "a".repeat(64),
                    AgentEventKind::ModelContextCommitted { digest, .. } if context => {
                        *digest = "b".repeat(64)
                    }
                    _ => {}
                }
            }
            assert!(NativeAutomationResultV1::derive(&mixed, &checkpoints).is_err());
            assert!(original.verify_record_prefix(&mixed).is_err());
        }
        let mut duplicate = record.clone();
        duplicate.events.push(AgentEvent::new(
            &id,
            original.last_sequence() + 1,
            AgentEventKind::TurnCompleted {
                response: "Fake completion".into(),
                steps: 1,
            },
        ));
        assert!(NativeAutomationResultV1::derive(&duplicate, &checkpoints).is_err());
        assert_eq!(
            store.load_strict(&id).unwrap().unwrap().events,
            record.events
        );
    }
}
