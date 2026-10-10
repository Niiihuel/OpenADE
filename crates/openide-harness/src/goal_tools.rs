//! Native goal tools scoped to one admitted turn. User activation and
//! pause/resume remain host actions; tools cannot create or shrink objectives.
use crate::{AgentError, ToolContext, ToolDefinition, ToolExecutor, ToolOutput};
use async_trait::async_trait;
use openide_core::{
    AgentEventKind, NativeGoal, NativeGoalCommitReceipt, NativeGoalEvidence, NativeGoalMutation,
    NativeGoalStore, NativeSessionLease, SessionStore, ToolCall, TurnMode,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

const NAMES: [&str; 3] = ["get_goal", "record_goal_evidence", "complete_goal"];
pub struct NativeGoalToolExecutor {
    base: Arc<dyn ToolExecutor>,
    sessions: Arc<SessionStore>,
    goals: NativeGoalStore,
    lease: Mutex<Option<Arc<NativeSessionLease>>>,
    turn_id: String,
    mode: TurnMode,
    recovery: bool,
}
impl NativeGoalToolExecutor {
    pub fn new(
        base: Arc<dyn ToolExecutor>,
        sessions: Arc<SessionStore>,
        lease: Arc<NativeSessionLease>,
        turn_id: &str,
    ) -> Result<Self, AgentError> {
        Self::bind(base, sessions, lease, turn_id, false)
    }
    /// Trusted host entry after replay has established a recoverable turn and
    /// the user selected Resume. A new wrapper may service a failed/cancelled
    /// admission; old normal-run wrappers remain revoked. Completed admissions
    /// never regain authority. This does not reconcile uncertain tool results.
    pub fn new_for_recovery(
        base: Arc<dyn ToolExecutor>,
        sessions: Arc<SessionStore>,
        lease: Arc<NativeSessionLease>,
        turn_id: &str,
    ) -> Result<Self, AgentError> {
        Self::bind(base, sessions, lease, turn_id, true)
    }
    fn bind(
        base: Arc<dyn ToolExecutor>,
        sessions: Arc<SessionStore>,
        lease: Arc<NativeSessionLease>,
        turn_id: &str,
        recovery: bool,
    ) -> Result<Self, AgentError> {
        sessions
            .native_session_for_lease(&lease)
            .map_err(|e| AgentError::InvalidRequest(e.to_string()))?;
        if base
            .definitions()
            .iter()
            .any(|definition| NAMES.contains(&definition.name.as_str()))
        {
            return Err(AgentError::InvalidRequest(
                "base executor already defines native goal tools".into(),
            ));
        }
        let mode = Self::active_mode(&sessions, &lease, turn_id, recovery)?;
        Ok(Self {
            base,
            goals: NativeGoalStore::new(sessions.clone()),
            sessions,
            lease: Mutex::new(Some(lease)),
            turn_id: turn_id.into(),
            mode,
            recovery,
        })
    }
    fn active_mode(
        sessions: &SessionStore,
        lease: &NativeSessionLease,
        turn_id: &str,
        recovery: bool,
    ) -> Result<TurnMode, AgentError> {
        let stored = sessions
            .load(lease.session_id())
            .map_err(|e| AgentError::Runtime(e.to_string()))?
            .ok_or_else(|| AgentError::InvalidRequest("unknown native session".into()))?;
        let mut admitted = None;
        let mut terminal = false;
        let mut completed = false;
        for event in &stored.events {
            match &event.kind {
                AgentEventKind::TurnAdmitted { turn_id, mode, .. }
                | AgentEventKind::TurnAdmittedWithImages { turn_id, mode, .. } => {
                    admitted = Some((turn_id.as_str(), *mode));
                    terminal = false;
                    completed = false;
                }
                AgentEventKind::TurnCompleted { .. } => {
                    terminal = true;
                    completed = true;
                }
                AgentEventKind::TurnFailed { .. } | AgentEventKind::TurnCancelled => {
                    terminal = true
                }
                _ => {}
            }
        }
        let Some((_, mode)) = admitted
            .filter(|(saved, _)| *saved == turn_id && !completed && (!terminal || recovery))
        else {
            return Err(AgentError::InvalidRequest(
                "goal tools require the current unfinished native admission".into(),
            ));
        };
        Ok(mode)
    }
    pub fn shutdown(&self) {
        self.lease.lock().unwrap_or_else(|p| p.into_inner()).take();
    }
    fn operation_id(&self, call: &ToolCall) -> Result<String, ToolOutput> {
        if call.id.is_empty() || call.id.len() > 256 || call.id.chars().any(char::is_control) {
            return Err(ToolOutput::error("invalid goal tool call ID"));
        }
        let mut hash = Sha256::new();
        hash.update(b"openide-goal-tool-v1\0");
        for field in [self.turn_id.as_bytes(), call.id.as_bytes()] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field);
        }
        Ok(format!("tool-{:x}", hash.finalize()))
    }
    fn output(goal: Option<NativeGoal>) -> ToolOutput {
        let view=goal.map(|g|json!({"goal_id":g.id,"objective":g.objective,"status":g.status,"revision":g.revision,"criteria":g.criteria}));
        ToolOutput::success(json!({"goal":view}).to_string())
    }
    fn parse_mutation(
        call: &ToolCall,
    ) -> Result<(String, u64, NativeGoalMutation), serde_json::Error> {
        match call.name.as_str() {
            "record_goal_evidence" => {
                serde_json::from_value::<EvidenceArgs>(call.arguments.clone()).map(|a| {
                    (
                        a.goal_id,
                        a.expected_revision,
                        NativeGoalMutation::RecordEvidence {
                            criterion_id: a.criterion_id,
                            evidence: NativeGoalEvidence {
                                turn_id: a.turn_id,
                                summary: a.summary,
                            },
                        },
                    )
                })
            }
            "complete_goal" => serde_json::from_value::<CompleteArgs>(call.arguments.clone())
                .map(|a| (a.goal_id, a.expected_revision, NativeGoalMutation::Complete)),
            _ => unreachable!("only native goal mutation names are parsed"),
        }
    }
    /// Host recovery inspection only. Does not replay a call or manufacture a
    /// missing ToolFinished event. The journal/manual recovery gate remains in
    /// force, even when an exact durable receipt is found.
    pub fn committed_mutation(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<Option<NativeGoalCommitReceipt>, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if !matches!(call.name.as_str(), "record_goal_evidence" | "complete_goal") {
            return Err(AgentError::InvalidRequest(
                "not a native goal mutation".into(),
            ));
        }
        let lease = self.authority()?;
        if !self.workspace_matches(&lease, context)? {
            return Err(AgentError::InvalidRequest(
                "goal tool workspace does not match native session".into(),
            ));
        }
        if self.mode != TurnMode::Build {
            return Err(AgentError::InvalidRequest(
                "native goal mutations are Build-only".into(),
            ));
        }
        let operation = self
            .operation_id(call)
            .map_err(|e| AgentError::InvalidRequest(e.content))?;
        let (id, revision, mutation) =
            Self::parse_mutation(call).map_err(|e| AgentError::InvalidRequest(e.to_string()))?;
        self.goals
            .committed_update(&lease, &id, &operation, revision, &mutation)
            .map_err(|e| AgentError::Runtime(e.to_string()))
    }
    fn authority(&self) -> Result<Arc<NativeSessionLease>, AgentError> {
        let lease = self
            .lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| AgentError::Runtime("native goal tool authority is closed".into()))?;
        self.sessions
            .native_session_for_lease(&lease)
            .map_err(|e| AgentError::Runtime(e.to_string()))?;
        Self::active_mode(&self.sessions, &lease, &self.turn_id, self.recovery)?;
        Ok(lease)
    }
    fn workspace_matches(
        &self,
        lease: &NativeSessionLease,
        context: &ToolContext,
    ) -> Result<bool, AgentError> {
        let session = self
            .sessions
            .native_session_for_lease(lease)
            .map_err(|e| AgentError::Runtime(e.to_string()))?;
        let workspace = std::fs::canonicalize(&context.workspace)
            .map_err(|e| AgentError::InvalidRequest(e.to_string()))?;
        Ok(std::fs::canonicalize(&session.workspace)
            .map_err(|e| AgentError::Runtime(e.to_string()))?
            == workspace)
    }
    fn execute_goal(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        let lease = self.authority()?;
        if !self.workspace_matches(&lease, context)? {
            return Ok(ToolOutput::error(
                "goal tool workspace does not match native session",
            ));
        }
        if call.name == "get_goal" {
            if call.arguments != json!({}) {
                return Ok(ToolOutput::error("get_goal takes an empty object"));
            }
            return Ok(match self.goals.list(lease.session_id()) {
                Ok(mut goals) => Self::output(goals.pop()),
                Err(error) => ToolOutput::error(error.to_string()),
            });
        }
        if self.mode != TurnMode::Build {
            return Ok(ToolOutput::error("native goal mutations are Build-only"));
        }
        let operation = match self.operation_id(call) {
            Ok(id) => id,
            Err(error) => return Ok(error),
        };
        let parsed = Self::parse_mutation(call);
        let (id, revision, mutation) = match parsed {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "invalid native goal arguments: {error}"
                )))
            }
        };
        Ok(
            match self
                .goals
                .update(&lease, &id, &operation, revision, mutation)
            {
                Ok(goal) => Self::output(Some(goal)),
                Err(error) => ToolOutput::error(error.to_string()),
            },
        )
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceArgs {
    goal_id: String,
    expected_revision: u64,
    criterion_id: String,
    turn_id: String,
    summary: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteArgs {
    goal_id: String,
    expected_revision: u64,
}
pub(crate) fn definitions(mode: TurnMode) -> Vec<ToolDefinition> {
    let mut definitions=vec![ToolDefinition{name:"get_goal".into(),description:"Read the current explicit native goal and all criteria; does not create, pause or resume goals.".into(),parameters:json!({"type":"object","properties":{},"additionalProperties":false})}];
    if mode == TurnMode::Build {
        definitions.push(ToolDefinition{name:"record_goal_evidence".into(),description:"Associate one criterion with a completed prior turn in this native session. A summary is an evidence assertion, not an independent verifier. Preserve the full objective and verify real behavior.".into(),parameters:json!({"type":"object","properties":{"goal_id":{"type":"string"},"expected_revision":{"type":"integer","minimum":0},"criterion_id":{"type":"string"},"turn_id":{"type":"string"},"summary":{"type":"string"}},"required":["goal_id","expected_revision","criterion_id","turn_id","summary"],"additionalProperties":false})});
        definitions.push(ToolDefinition{name:"complete_goal".into(),description:"Request completion only after verifying every criterion. Rejects missing evidence or a paused goal; successful turns alone do not prove criterion satisfaction.".into(),parameters:json!({"type":"object","properties":{"goal_id":{"type":"string"},"expected_revision":{"type":"integer","minimum":0}},"required":["goal_id","expected_revision"],"additionalProperties":false})});
    }
    definitions
}
#[async_trait]
impl ToolExecutor for NativeGoalToolExecutor {
    async fn execute_streaming(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        output: crate::ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if NAMES.contains(&call.name.as_str()) {
            self.execute(call, context).await
        } else {
            self.base.execute_streaming(call, context, output).await
        }
    }

    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut result = self.base.definitions();
        result.extend(definitions(self.mode));
        result
    }
    fn certifies_plan_read(&self, name: &str) -> bool {
        if NAMES.contains(&name) {
            name == "get_goal"
        } else {
            self.base.certifies_plan_read(name)
        }
    }
    fn certifies_parallel_read(&self, name: &str) -> bool {
        if NAMES.contains(&name) {
            false
        } else {
            self.base.certifies_parallel_read(name)
        }
    }
    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if NAMES.contains(&call.name.as_str()) {
            self.execute_goal(call, context)
        } else {
            self.base.execute(call, context).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalTools;
    use openide_core::{NativeGoalCriterion, NativeGoalStatus};
    use tokio_util::sync::CancellationToken;
    fn call(id: &str, name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
    use serde_json::Value;
    #[tokio::test]
    async fn goal_tools_enforce_plan_scope_arguments_cancellation_and_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
        let goals = NativeGoalStore::new(sessions.clone());
        goals
            .create(
                &lease,
                "goal",
                "Objective",
                vec![NativeGoalCriterion {
                    id: "one".into(),
                    description: "Verify".into(),
                    evidence: None,
                }],
            )
            .unwrap();
        sessions
            .admit_native_turn(&session.id, "plan", "Inspect", "fixture", TurnMode::Plan)
            .unwrap();
        assert!(NativeGoalToolExecutor::new(
            Arc::new(LocalTools::new()),
            sessions.clone(),
            lease.clone(),
            "wrong-turn"
        )
        .is_err());
        let tools = Arc::new(
            NativeGoalToolExecutor::new(
                Arc::new(LocalTools::new()),
                sessions.clone(),
                lease.clone(),
                "plan",
            )
            .unwrap(),
        );
        assert!(tools.definitions().iter().any(|d| d.name == "get_goal"));
        assert!(!tools
            .definitions()
            .iter()
            .any(|d| d.name == "complete_goal" || d.name == "record_goal_evidence"));
        assert!(NativeGoalToolExecutor::new(
            tools.clone(),
            sessions.clone(),
            lease.clone(),
            "plan"
        )
        .is_err());
        let context = ToolContext {
            workspace: root.path().into(),
            cancel: CancellationToken::new(),
        };
        let read = tools
            .execute(&call("read", "get_goal", json!({})), &context)
            .await
            .unwrap();
        assert!(!read.is_error);
        assert!(read.content.contains("Objective"));
        assert!(
            tools
                .execute(
                    &call("bad-read", "get_goal", json!({"session_id":"other"})),
                    &context
                )
                .await
                .unwrap()
                .is_error
        );
        assert!(
            tools
                .execute(
                    &call(
                        "close",
                        "complete_goal",
                        json!({"goal_id":"goal","expected_revision":0})
                    ),
                    &context
                )
                .await
                .unwrap()
                .is_error
        );
        let foreign = tempfile::tempdir().unwrap();
        let bad_context = ToolContext {
            workspace: foreign.path().into(),
            cancel: CancellationToken::new(),
        };
        assert!(
            tools
                .execute(&call("foreign", "get_goal", json!({})), &bad_context)
                .await
                .unwrap()
                .is_error
        );
        context.cancel.cancel();
        assert!(matches!(
            tools
                .execute(&call("cancelled", "get_goal", json!({})), &context)
                .await,
            Err(AgentError::Cancelled)
        ));
        tools.shutdown();
        let context = ToolContext {
            workspace: root.path().into(),
            cancel: CancellationToken::new(),
        };
        assert!(tools
            .execute(&call("closed", "get_goal", json!({})), &context)
            .await
            .is_err());
        let saved = goals.list(&session.id).unwrap();
        assert_eq!(saved[0].revision, 0);
        assert_eq!(saved[0].status, NativeGoalStatus::Active);
    }
    #[tokio::test]
    async fn build_goal_tools_reject_unknown_evidence_and_stale_turn_authority_without_mutation() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
        let goals = NativeGoalStore::new(sessions.clone());
        goals
            .create(
                &lease,
                "goal",
                "Objective",
                vec![NativeGoalCriterion {
                    id: "one".into(),
                    description: "Verify".into(),
                    evidence: None,
                }],
            )
            .unwrap();
        let admission = sessions
            .admit_native_turn(&session.id, "build", "Build", "fixture", TurnMode::Build)
            .unwrap();
        let tools = NativeGoalToolExecutor::new(
            Arc::new(LocalTools::new()),
            sessions.clone(),
            lease.clone(),
            "build",
        )
        .unwrap();
        let context = ToolContext {
            workspace: root.path().into(),
            cancel: CancellationToken::new(),
        };
        for request in [
            call(
                "premature",
                "complete_goal",
                json!({"goal_id":"goal","expected_revision":0}),
            ),
            call(
                "unknown",
                "record_goal_evidence",
                json!({"goal_id":"goal","expected_revision":0,"criterion_id":"one","turn_id":"build","summary":"Not completed yet"}),
            ),
            call(
                "pause-injection",
                "complete_goal",
                json!({"goal_id":"goal","expected_revision":0,"status":"paused"}),
            ),
            call(
                "foreign-goal",
                "complete_goal",
                json!({"goal_id":"other","expected_revision":0}),
            ),
        ] {
            assert!(tools.execute(&request, &context).await.unwrap().is_error);
        }
        sessions
            .append_native_event(&openide_core::AgentEvent::new(
                &session.id,
                admission.sequence + 1,
                AgentEventKind::TurnStarted {
                    prompt: "Build".into(),
                },
            ))
            .unwrap();
        sessions
            .append_native_event(&openide_core::AgentEvent::new(
                &session.id,
                admission.sequence + 2,
                AgentEventKind::TurnCompleted {
                    response: "Fixture".into(),
                    steps: 1,
                },
            ))
            .unwrap();
        assert!(tools
            .execute(&call("stale", "get_goal", json!({})), &context)
            .await
            .is_err());
        assert!(NativeGoalToolExecutor::new(
            Arc::new(LocalTools::new()),
            sessions.clone(),
            lease.clone(),
            "build"
        )
        .is_err());
        assert!(NativeGoalToolExecutor::new_for_recovery(
            Arc::new(LocalTools::new()),
            sessions.clone(),
            lease.clone(),
            "build"
        )
        .is_err());
        sessions
            .admit_native_turn(&session.id, "next", "Next", "fixture", TurnMode::Build)
            .unwrap();
        let next = NativeGoalToolExecutor::new(
            Arc::new(LocalTools::new()),
            sessions.clone(),
            lease.clone(),
            "next",
        )
        .unwrap();
        assert_ne!(
            tools
                .operation_id(&call("same", "complete_goal", json!({})))
                .unwrap(),
            next.operation_id(&call("same", "complete_goal", json!({})))
                .unwrap()
        );
        assert_eq!(goals.list(&session.id).unwrap()[0].revision, 0);
    }
    #[tokio::test]
    async fn explicit_recovery_gets_fresh_authority_after_failure_or_cancel_but_not_later_admission(
    ) {
        for terminal in [
            AgentEventKind::TurnFailed {
                error: "lost result".into(),
            },
            AgentEventKind::TurnCancelled,
        ] {
            let root = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
            let session = sessions.create(root.path(), "native").unwrap();
            let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
            let goals = NativeGoalStore::new(sessions.clone());
            goals
                .create(
                    &lease,
                    "goal",
                    "Full objective",
                    vec![NativeGoalCriterion {
                        id: "verify".into(),
                        description: "Verify".into(),
                        evidence: None,
                    }],
                )
                .unwrap();
            let admission = sessions
                .admit_native_turn(&session.id, "build", "Build", "fixture", TurnMode::Build)
                .unwrap();
            let old = NativeGoalToolExecutor::new(
                Arc::new(LocalTools::new()),
                sessions.clone(),
                lease.clone(),
                "build",
            )
            .unwrap();
            sessions
                .append_native_event(&openide_core::AgentEvent::new(
                    &session.id,
                    admission.sequence + 1,
                    AgentEventKind::TurnStarted {
                        prompt: "Build".into(),
                    },
                ))
                .unwrap();
            sessions
                .append_native_event(&openide_core::AgentEvent::new(
                    &session.id,
                    admission.sequence + 2,
                    terminal,
                ))
                .unwrap();
            let context = ToolContext {
                workspace: root.path().into(),
                cancel: CancellationToken::new(),
            };
            assert!(old
                .execute(&call("old", "get_goal", json!({})), &context)
                .await
                .is_err());
            assert!(NativeGoalToolExecutor::new(
                Arc::new(LocalTools::new()),
                sessions.clone(),
                lease.clone(),
                "build"
            )
            .is_err());
            let fresh = NativeGoalToolExecutor::new_for_recovery(
                Arc::new(LocalTools::new()),
                sessions.clone(),
                lease.clone(),
                "build",
            )
            .unwrap();
            assert!(
                !fresh
                    .execute(&call("fresh", "get_goal", json!({})), &context)
                    .await
                    .unwrap()
                    .is_error
            );
            sessions
                .admit_native_turn(&session.id, "next", "Next", "fixture", TurnMode::Build)
                .unwrap();
            assert!(fresh
                .execute(&call("stale", "get_goal", json!({})), &context)
                .await
                .is_err());
            assert_eq!(goals.list(&session.id).unwrap()[0].revision, 0);
        }
    }
}
