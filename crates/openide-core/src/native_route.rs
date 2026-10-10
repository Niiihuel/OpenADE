//! Bounded, versioned model route metadata without endpoint or credential text.

use crate::{AgentEventKind, AgentMessage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MODEL_ROUTE_VERSION: u16 = 1;
pub const MAX_MODEL_ROUTE_PROVIDER_BYTES: usize = 128;
pub const MAX_MODEL_ROUTE_MODEL_BYTES: usize = 512;
pub const MAX_MODEL_ROUTE_ENDPOINT_BYTES: usize = 16 * 1024;

/// Hash the exact endpoint identity used by the host. This does not normalize,
/// fetch, or expose the URL, and deliberately has a separate digest domain.
pub fn native_model_endpoint_digest(endpoint: &str) -> Option<String> {
    if endpoint.trim().is_empty() || endpoint.len() > MAX_MODEL_ROUTE_ENDPOINT_BYTES {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"openide-native-model-route-endpoint\0");
    digest.update(MODEL_ROUTE_VERSION.to_be_bytes());
    digest.update((endpoint.len() as u64).to_be_bytes());
    digest.update(endpoint.as_bytes());
    Some(format!("{:x}", digest.finalize()))
}

/// The model backend's stable request profile already includes its wire API.
/// This is the exact fingerprint in canonical lowercase hexadecimal.
pub fn model_request_profile_digest(profile: [u8; 32]) -> String {
    profile.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelRouteSnapshot {
    pub version: u16,
    pub provider_id: String,
    pub model_id: String,
    pub endpoint_digest: String,
    pub request_profile_digest: String,
}

impl ModelRouteSnapshot {
    pub fn new(
        provider_id: &str,
        model_id: &str,
        endpoint: &str,
        profile: [u8; 32],
    ) -> Option<Self> {
        let route = Self {
            version: MODEL_ROUTE_VERSION,
            provider_id: provider_id.into(),
            model_id: model_id.into(),
            endpoint_digest: native_model_endpoint_digest(endpoint)?,
            request_profile_digest: model_request_profile_digest(profile),
        };
        route.is_valid().then_some(route)
    }

    pub fn is_valid(&self) -> bool {
        fn id(value: &str, max: usize) -> bool {
            !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        }
        fn digest(value: &str) -> bool {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }
        self.version == MODEL_ROUTE_VERSION
            && id(&self.provider_id, MAX_MODEL_ROUTE_PROVIDER_BYTES)
            && id(&self.model_id, MAX_MODEL_ROUTE_MODEL_BYTES)
            && digest(&self.endpoint_digest)
            && digest(&self.request_profile_digest)
    }

    pub fn from_event(kind: &AgentEventKind) -> Option<Self> {
        if let AgentEventKind::ModelRouteCommitted {
            version,
            provider_id,
            model_id,
            endpoint_digest,
            request_profile_digest,
        } = kind
        {
            Some(Self {
                version: *version,
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                endpoint_digest: endpoint_digest.clone(),
                request_profile_digest: request_profile_digest.clone(),
            })
        } else {
            None
        }
    }

    pub fn into_event(self) -> AgentEventKind {
        AgentEventKind::ModelRouteCommitted {
            version: self.version,
            provider_id: self.provider_id,
            model_id: self.model_id,
            endpoint_digest: self.endpoint_digest,
            request_profile_digest: self.request_profile_digest,
        }
    }
}

/// Retain portable conversation content while moving to a different model
/// route. Provider output items are transport/profile-specific references and
/// may contain opaque reasoning data which another route cannot interpret.
/// Unknown or invalid route identity also prevents reusing those items.
///
/// Only the materialized history is changed; callers retain the original
/// committed events. Text, tool calls/results and images remain untouched.
/// Returns whether any provider output items were removed.
pub fn reconcile_model_history_route(
    history: &mut [AgentMessage],
    previous: Option<&ModelRouteSnapshot>,
    next: &ModelRouteSnapshot,
) -> bool {
    if next.is_valid() && previous == Some(next) {
        return false;
    }
    let mut changed = false;
    for message in history {
        if !message.provider_output_items.is_empty() {
            message.provider_output_items.clear();
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        replay_native, AgentEvent, AgentMessage, CheckpointCompatibility, EventStore,
        JsonlSessionStore, MemorySessionStore, ReplayError, SessionStore, StoreError, TurnMode,
        MODEL_CONTEXT_VERSION,
    };

    fn compatibility() -> CheckpointCompatibility {
        CheckpointCompatibility {
            provider: "custom-exact-provider".into(),
            model: "model/exact:id".into(),
            context_format_key: "fixture-v1".into(),
        }
    }

    fn events(session_id: &str) -> Vec<AgentEvent> {
        let route = ModelRouteSnapshot::new(
            "custom-exact-provider",
            "model/exact:id",
            "https://private.example/v1?configured=exact",
            [17; 32],
        )
        .unwrap();
        [
            AgentEventKind::TurnAdmitted {
                turn_id: "route-turn".into(),
                prompt: "Fixture".into(),
                model: "model/exact:id".into(),
                mode: TurnMode::Build,
            },
            AgentEventKind::TurnStarted {
                prompt: "Fixture".into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: "a".repeat(64),
            },
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: "b".repeat(64),
            },
            route.into_event(),
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "model/exact:id".into(),
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| AgentEvent::new(session_id, index as u64 + 1, kind))
        .collect()
    }

    #[test]
    fn model_route_snapshot_is_exact_bounded_private_and_legacy_optional() {
        let route = ModelRouteSnapshot::new(
            "provider/ñ",
            "arbitrary-model:exact",
            "http://user:private-secret@local.example/path?q=private",
            [7; 32],
        )
        .unwrap();
        let encoded = serde_json::to_string(&route.clone().into_event()).unwrap();
        assert!(encoded.contains("provider/ñ"));
        assert!(encoded.contains("arbitrary-model:exact"));
        assert!(!encoded.contains("private-secret"));
        assert!(!encoded.contains("local.example"));
        assert_eq!(route.request_profile_digest, "07".repeat(32));
        assert_ne!(
            native_model_endpoint_digest("https://api.example/v1"),
            native_model_endpoint_digest("https://api.example/v1/")
        );
        assert!(ModelRouteSnapshot::new("", "model", "endpoint", [0; 32]).is_none());
        assert!(ModelRouteSnapshot::new("provider", "bad\nmodel", "endpoint", [0; 32]).is_none());
        assert!(ModelRouteSnapshot::new(
            &"p".repeat(MAX_MODEL_ROUTE_PROVIDER_BYTES + 1),
            "model",
            "endpoint",
            [0; 32]
        )
        .is_none());
        assert!(ModelRouteSnapshot::new(
            "provider",
            &"m".repeat(MAX_MODEL_ROUTE_MODEL_BYTES + 1),
            "endpoint",
            [0; 32]
        )
        .is_none());
        assert!(
            native_model_endpoint_digest(&"x".repeat(MAX_MODEL_ROUTE_ENDPOINT_BYTES + 1)).is_none()
        );
        let modern = events("session");
        assert!(replay_native("session", &modern, None, &compatibility())
            .unwrap()
            .active_model_route
            .is_some());
        let mut legacy = modern;
        legacy.remove(4);
        legacy[4].sequence = 5;
        assert!(
            replay_native("session", &legacy, None, &compatibility())
                .unwrap()
                .active_model_route
                .is_none(),
            "legacy context must not acquire a current selection's attribution"
        );
    }

    #[test]
    fn model_route_snapshot_replay_rejects_wrong_order_identity_version_model_and_hash_shape() {
        let valid = events("session");
        for variant in 0..7 {
            let mut broken = valid.clone();
            match variant {
                0 => {
                    if let AgentEventKind::ModelRouteCommitted { version, .. } = &mut broken[4].kind
                    {
                        *version += 1;
                    }
                }
                1 => {
                    if let AgentEventKind::ModelRouteCommitted {
                        endpoint_digest, ..
                    } = &mut broken[4].kind
                    {
                        *endpoint_digest = "A".repeat(64);
                    }
                }
                2 => {
                    if let AgentEventKind::ModelRouteCommitted { model_id, .. } =
                        &mut broken[4].kind
                    {
                        *model_id = "another-exact-model".into();
                    }
                }
                3 => broken[4].session_id = "foreign-session".into(),
                4 => {
                    broken.swap(3, 4);
                    broken[3].sequence = 4;
                    broken[4].sequence = 5;
                }
                5 => {
                    broken.swap(4, 5);
                    broken[4].sequence = 5;
                    broken[5].sequence = 6;
                }
                _ => {
                    broken.insert(5, broken[4].clone());
                    for (index, event) in broken.iter_mut().enumerate() {
                        event.sequence = index as u64 + 1;
                    }
                }
            }
            assert!(
                matches!(
                    replay_native("session", &broken, None, &compatibility()),
                    Err(ReplayError::Conflict { .. } | ReplayError::SessionMismatch(_))
                ),
                "invalid route accepted: {variant}"
            );
        }
    }

    #[test]
    fn model_route_snapshot_reopens_exact_metadata_and_rejects_corrupt_storage() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create(root.path(), "native").unwrap();
        let mut saved = events(&session.id);
        saved.extend([
            AgentEvent::new(
                &session.id,
                7,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("Done", vec![]),
                    response_id: None,
                    usage: None,
                },
            ),
            AgentEvent::new(
                &session.id,
                8,
                AgentEventKind::TurnCompleted {
                    response: "Done".into(),
                    steps: 1,
                },
            ),
        ]);
        let admission = store
            .admit_native_turn(
                &session.id,
                "route-turn",
                "Fixture",
                "model/exact:id",
                TurnMode::Build,
            )
            .unwrap();
        for (offset, event) in saved.iter().skip(1).enumerate() {
            store
                .append_native_event(&AgentEvent::new(
                    &session.id,
                    admission.sequence + offset as u64 + 1,
                    event.kind.clone(),
                ))
                .unwrap();
        }
        saved = store.load(&session.id).unwrap().unwrap().events;
        drop(store);
        let reopened = SessionStore::open(root.path()).unwrap();
        let actual = reopened.load(&session.id).unwrap().unwrap();
        assert_eq!(actual.events, saved);
        assert!(
            replay_native(&session.id, &actual.events, None, &compatibility())
                .unwrap()
                .active_model_route
                .is_none()
        );
        let route_index = saved
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ModelRouteCommitted { .. }))
            .unwrap();
        assert_eq!(
            ModelRouteSnapshot::from_event(&actual.events[route_index].kind),
            ModelRouteSnapshot::from_event(&saved[route_index].kind)
        );
        let mut bad = saved[route_index].clone();
        if let AgentEventKind::ModelRouteCommitted {
            request_profile_digest,
            ..
        } = &mut bad.kind
        {
            *request_profile_digest = "bad digest".into();
        }
        assert!(matches!(
            MemorySessionStore::new().append(&bad),
            Err(StoreError::InvalidModelRouteSnapshot)
        ));
        assert!(matches!(
            JsonlSessionStore::new(root.path().join("other"))
                .unwrap()
                .append(&bad),
            Err(StoreError::InvalidModelRouteSnapshot)
        ));
        let path = reopened
            .storage_root()
            .join(format!("{}.jsonl", session.id));
        let before = std::fs::read(&path).unwrap();
        let replaced = String::from_utf8(before)
            .unwrap()
            .replace(&"11".repeat(32), "INVALID");
        std::fs::write(&path, &replaced).unwrap();
        drop(reopened);
        assert!(SessionStore::open(root.path())
            .unwrap()
            .load(&session.id)
            .is_err());
        assert_eq!(
            std::fs::read(path).unwrap(),
            replaced.as_bytes(),
            "corrupt complete route record was silently repaired"
        );
    }

    fn history_fixture() -> Vec<AgentMessage> {
        let call = crate::ToolCall {
            id: "read-1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path":"src/lib.rs", "start_line":4}),
        };
        let image =
            crate::UserImage::from_bytes(crate::UserImageMime::Png, b"\x89PNG\r\n\x1a\n").unwrap();
        let mut assistant = AgentMessage::assistant("Voy a revisar el archivo.", vec![call]);
        assistant.provider_output_items =
            vec![serde_json::json!({"type":"reasoning", "encrypted_content":"opaque-route-a"})];
        vec![
            AgentMessage::user_with_images("Revisá esta imagen y el archivo", vec![image]).unwrap(),
            assistant,
            AgentMessage::tool("read-1", "4 pub fn saludo() {}\n"),
            AgentMessage::assistant("El archivo contiene una función.", Vec::new()),
        ]
    }

    #[test]
    fn route_reconciliation_preserves_canonical_messages_and_requires_exact_identity() {
        let route =
            ModelRouteSnapshot::new("provider-a", "model-a", "https://a.example/v1", [1; 32])
                .unwrap();
        let original = history_fixture();
        let mut same = original.clone();
        assert!(!reconcile_model_history_route(
            &mut same,
            Some(&route),
            &route
        ));
        assert_eq!(
            same, original,
            "the same route retains opaque stateless continuation data"
        );
        let mut portable = original.clone();
        for message in &mut portable {
            message.provider_output_items.clear();
        }
        for next in [
            ModelRouteSnapshot::new("provider-b", "model-a", "https://a.example/v1", [1; 32])
                .unwrap(),
            ModelRouteSnapshot::new("provider-a", "model-b", "https://a.example/v1", [1; 32])
                .unwrap(),
            ModelRouteSnapshot::new("provider-a", "model-a", "https://b.example/v1", [1; 32])
                .unwrap(),
            ModelRouteSnapshot::new("provider-a", "model-a", "https://a.example/v1", [2; 32])
                .unwrap(),
        ] {
            let mut migrated = original.clone();
            assert!(reconcile_model_history_route(
                &mut migrated,
                Some(&route),
                &next
            ));
            assert_eq!(
                migrated, portable,
                "text, call arguments/results, roles and images must not change"
            );
            assert!(
                !reconcile_model_history_route(&mut migrated, Some(&route), &next),
                "reconciliation is idempotent"
            );
        }
        let mut unknown = original.clone();
        assert!(reconcile_model_history_route(&mut unknown, None, &route));
        assert_eq!(
            unknown, portable,
            "unknown provenance is never attributed to the newly selected route"
        );
    }

    fn push_event(events: &mut Vec<AgentEvent>, kind: AgentEventKind) {
        events.push(AgentEvent::new("session", events.len() as u64 + 1, kind));
    }

    fn completed_tool_turn() -> (Vec<AgentEvent>, ModelRouteSnapshot) {
        let mut log = events("session");
        let route = ModelRouteSnapshot::from_event(&log[4].kind).unwrap();
        let mut history = history_fixture();
        log[0].kind = AgentEventKind::TurnAdmittedWithImages {
            turn_id: "route-turn".into(),
            prompt: "Fixture".into(),
            model: route.model_id.clone(),
            mode: TurnMode::Build,
            images: history[0].images.clone(),
        };
        let mut assistant = history.remove(1);
        assistant.content.clear();
        let call = assistant.tool_calls[0].clone();
        push_event(
            &mut log,
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant,
                response_id: Some("response-a-tool".into()),
                usage: None,
            },
        );
        push_event(
            &mut log,
            AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            },
        );
        push_event(
            &mut log,
            AgentEventKind::ToolFinished {
                tool_call_id: call.id,
                output: "4 pub fn saludo() {}\n".into(),
                is_error: false,
            },
        );
        push_event(
            &mut log,
            AgentEventKind::ModelRequestStarted {
                step: 2,
                model: route.model_id.clone(),
            },
        );
        let mut answer = AgentMessage::assistant("El archivo contiene una función.", Vec::new());
        answer.provider_output_items =
            vec![serde_json::json!({"type":"message", "id":"response-a-answer"})];
        push_event(
            &mut log,
            AgentEventKind::ModelResponseCommitted {
                step: 2,
                assistant: answer.clone(),
                response_id: Some("response-a-answer".into()),
                usage: None,
            },
        );
        push_event(
            &mut log,
            AgentEventKind::TurnCompleted {
                response: answer.content,
                steps: 2,
            },
        );
        (log, route)
    }

    fn begin_route_turn(
        log: &mut Vec<AgentEvent>,
        route: &ModelRouteSnapshot,
        id: &str,
        commit_route: bool,
    ) {
        push_event(
            log,
            AgentEventKind::TurnAdmitted {
                turn_id: id.into(),
                prompt: format!("Prompt {id}"),
                model: route.model_id.clone(),
                mode: TurnMode::Build,
            },
        );
        push_event(
            log,
            AgentEventKind::TurnStarted {
                prompt: format!("Prompt {id}"),
            },
        );
        push_event(
            log,
            AgentEventKind::ToolCatalogCommitted {
                digest: "a".repeat(64),
            },
        );
        push_event(
            log,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: "b".repeat(64),
            },
        );
        if commit_route {
            push_event(log, route.clone().into_event());
        }
        push_event(
            log,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: route.model_id.clone(),
            },
        );
    }

    #[test]
    fn replay_route_change_reconciles_requests_and_compaction_without_rewriting_tool_journal() {
        let (base, route_a) = completed_tool_turn();
        let before = replay_native("session", &base, None, &compatibility()).unwrap();
        assert!(before.can_start_new_turn);
        assert!(before.active_model_route.is_none());
        assert_eq!(before.history_model_route.as_ref(), Some(&route_a));
        assert_eq!(
            before
                .history
                .iter()
                .filter(|message| !message.provider_output_items.is_empty())
                .count(),
            2
        );
        let route_b =
            ModelRouteSnapshot::new("provider-b", "model-b", "https://b.example/v1", [2; 32])
                .unwrap();
        for next in [&route_a, &route_b] {
            let mut log = base.clone();
            begin_route_turn(&mut log, next, "turn-next", true);
            let durable_bytes = serde_json::to_vec(&log).unwrap();
            let replay = replay_native("session", &log, None, &compatibility()).unwrap();
            let mut expected = before.history.clone();
            if next != &route_a {
                for message in &mut expected {
                    message.provider_output_items.clear();
                }
            }
            expected.push(AgentMessage::user("Prompt turn-next"));
            assert_eq!(replay.history, expected);
            assert_eq!(
                replay
                    .committed_messages
                    .iter()
                    .map(|entry| entry.message.clone())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(replay.history_model_route.as_ref(), Some(next));
            assert_eq!(replay.active_model_route.as_ref(), Some(next));
            assert!(matches!(
                replay.recovery,
                crate::ReplayRecovery::ModelInFlight { step: 1, .. }
            ));
            assert!(replay.pending_tool_batch.is_none());
            assert_eq!(
                serde_json::to_vec(&log).unwrap(),
                durable_bytes,
                "replay does not rewrite raw provider evidence or tool events"
            );
        }
    }

    #[test]
    fn legacy_intervening_request_invalidates_historical_route_attribution() {
        let (mut log, route) = completed_tool_turn();
        begin_route_turn(&mut log, &route, "legacy-middle", false);
        let mut answer = AgentMessage::assistant("Legacy answer", Vec::new());
        answer.provider_output_items =
            vec![serde_json::json!({"type":"reasoning", "encrypted_content":"unknown-route"})];
        push_event(
            &mut log,
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: answer,
                response_id: None,
                usage: None,
            },
        );
        push_event(
            &mut log,
            AgentEventKind::TurnCompleted {
                response: "Legacy answer".into(),
                steps: 1,
            },
        );
        let legacy = replay_native("session", &log, None, &compatibility()).unwrap();
        assert!(legacy.history_model_route.is_none());
        assert!(legacy
            .history
            .iter()
            .any(|message| !message.provider_output_items.is_empty()));
        begin_route_turn(&mut log, &route, "known-again", true);
        let known = replay_native("session", &log, None, &compatibility()).unwrap();
        assert_eq!(known.history_model_route.as_ref(), Some(&route));
        assert!(known
            .history
            .iter()
            .all(|message| message.provider_output_items.is_empty()));
        assert!(known
            .history
            .iter()
            .any(|message| message.content == "Legacy answer"));
        assert_eq!(
            known
                .history
                .iter()
                .filter(|message| message.tool_call_id.as_deref() == Some("read-1"))
                .count(),
            1
        );
    }
}
