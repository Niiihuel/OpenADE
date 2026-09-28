use crate::permission::PendingPermissions;
use crate::{
    AgentError, ContextPolicy, ModelBackend, PermissionChoice, PermissionMode, PermissionPolicy,
    ToolContext, ToolDefinition, ToolExecutor,
};
use openide_core::{
    AgentEvent, AgentEventKind, AgentMessage, AgentRunRequest, PermissionResolution, ToolCall,
};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub struct RunLimits {
    pub max_model_steps: usize,
    pub max_tool_calls: usize,
    pub max_tool_output_chars: usize,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_model_steps: 24,
            max_tool_calls: 64,
            max_tool_output_chars: 16_000,
        }
    }
}

#[derive(Clone)]
pub struct AgentRunner {
    model: Arc<dyn ModelBackend>,
    tools: Arc<dyn ToolExecutor>,
    limits: RunLimits,
    context_policy: ContextPolicy,
    permission_policy: PermissionPolicy,
    system_instructions: String,
}

pub struct AgentRunHandle {
    pub session_id: String,
    pub events: mpsc::Receiver<AgentEvent>,
    cancel: CancellationToken,
    pending_permissions: Arc<PendingPermissions>,
}

impl AgentRunHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Resolve a permission request emitted by this run. Returns false if the
    /// request already settled or never belonged to this handle.
    pub fn resolve_permission(&self, request_id: &str, choice: PermissionChoice) -> bool {
        self.pending_permissions.resolve(request_id, choice)
    }
}

impl AgentRunner {
    pub fn new(model: Arc<dyn ModelBackend>, tools: Arc<dyn ToolExecutor>) -> Self {
        Self {
            model,
            tools,
            limits: RunLimits::default(),
            context_policy: ContextPolicy::default(),
            permission_policy: PermissionPolicy::default(),
            system_instructions: String::new(),
        }
    }

    pub fn with_limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_context_policy(mut self, policy: ContextPolicy) -> Self {
        self.context_policy = policy;
        self
    }

    pub fn with_permission_policy(mut self, policy: PermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    /// Adds workspace capabilities to the model's system context without
    /// changing the user prompt recorded in the session event log.
    pub fn with_system_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.system_instructions = instructions.into();
        self
    }

    /// Starts a worker thread. Callers may poll `handle.events` from a synchronous UI loop.
    pub fn start(&self, request: AgentRunRequest) -> AgentRunHandle {
        let session_id = request
            .session_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let cancel = CancellationToken::new();
        let pending_permissions = Arc::new(PendingPermissions::default());
        let (sender, receiver) = mpsc::channel();
        let worker = self.clone();
        let worker_cancel = cancel.clone();
        let worker_pending = pending_permissions.clone();
        let worker_session_id = session_id.clone();
        std::thread::spawn(move || {
            let mut sink = EventSink {
                session_id: worker_session_id,
                sequence: request.sequence_start,
                sender,
            };
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = sink.emit(AgentEventKind::TurnFailed {
                        error: format!("cannot start async runtime: {error}"),
                    });
                    return;
                }
            };
            let result = runtime.block_on(worker.run_inner(
                request,
                worker_cancel,
                worker_pending,
                &mut sink,
            ));
            match result {
                Ok(()) => {}
                Err(AgentError::Cancelled) => {
                    let _ = sink.emit(AgentEventKind::TurnCancelled);
                }
                Err(error) => {
                    let _ = sink.emit(AgentEventKind::TurnFailed {
                        error: error.to_string(),
                    });
                }
            }
        });
        AgentRunHandle {
            session_id,
            events: receiver,
            cancel,
            pending_permissions,
        }
    }

    async fn run_inner(
        &self,
        request: AgentRunRequest,
        cancel: CancellationToken,
        pending_permissions: Arc<PendingPermissions>,
        sink: &mut EventSink,
    ) -> Result<(), AgentError> {
        sink.emit(AgentEventKind::SessionStarted {
            workspace: request.workspace.clone(),
        })?;
        sink.emit(AgentEventKind::TurnStarted {
            prompt: request.prompt.clone(),
        })?;
        if request.prompt.trim().is_empty() {
            return Err(AgentError::InvalidRequest("prompt is empty".into()));
        }
        let workspace = canonical_workspace(&request.workspace)?;
        let definitions = self.tools.definitions();
        let available_tools = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut system = format!(
            "You are the OpenIDE coding agent. The active workspace is {}. Available tools: {}. Inspect files before editing when needed. Tool paths are relative to this workspace. Report only actions confirmed by tool results, and explain failures plainly.",
            workspace.display(),
            if available_tools.is_empty() { "none" } else { &available_tools }
        );
        if !self.system_instructions.trim().is_empty() {
            system.push_str("\n\nActive workspace skills:\n");
            system.push_str(&self.system_instructions);
        }
        let mut messages = vec![AgentMessage::system(system)];
        messages.extend(request.history);
        let mut current_prompt_index = messages.len();
        messages.push(AgentMessage::user(request.prompt));
        let mut tool_count = 0usize;
        for step in 1..=self.limits.max_model_steps {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            self.context_policy
                .compact(&mut messages, &mut current_prompt_index)?;
            sink.emit(AgentEventKind::ModelRequestStarted {
                step,
                model: self.model.model_id().to_owned(),
            })?;
            let output = self
                .model_step(&messages, &definitions, cancel.clone(), sink)
                .await?;
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if output.tool_calls.is_empty() {
                if output.content.trim().is_empty() {
                    return Err(AgentError::Model(
                        "model returned neither text nor a tool call".into(),
                    ));
                }
                sink.emit(AgentEventKind::AssistantMessage {
                    content: output.content.clone(),
                })?;
                sink.emit(AgentEventKind::TurnCompleted {
                    response: output.content,
                    steps: step,
                })?;
                return Ok(());
            }
            if tool_count.saturating_add(output.tool_calls.len()) > self.limits.max_tool_calls {
                return Err(AgentError::Limit(format!(
                    "more than {} tool calls in one run",
                    self.limits.max_tool_calls
                )));
            }
            if !output.content.is_empty() {
                sink.emit(AgentEventKind::AssistantMessage {
                    content: output.content.clone(),
                })?;
            }
            messages.push(AgentMessage::assistant(
                output.content,
                output.tool_calls.clone(),
            ));
            for call in output.tool_calls {
                if cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                tool_count += 1;
                sink.emit(AgentEventKind::ToolStarted {
                    tool_call: call.clone(),
                })?;
                let context = ToolContext {
                    workspace: workspace.clone(),
                    cancel: cancel.clone(),
                };
                let permission_result = self
                    .check_permission(&call, &cancel, &pending_permissions, sink)
                    .await?;
                if cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                let result = match permission_result {
                    Some(denied) => Ok(denied),
                    None => self.tools.execute(&call, &context).await,
                };
                let result = match result {
                    Ok(result) => result,
                    Err(AgentError::Cancelled) => return Err(AgentError::Cancelled),
                    Err(error) => crate::ToolOutput::error(error.to_string()),
                };
                let content = truncate_chars(&result.content, self.limits.max_tool_output_chars);
                sink.emit(AgentEventKind::ToolFinished {
                    tool_call_id: call.id.clone(),
                    output: content.clone(),
                    is_error: result.is_error,
                })?;
                messages.push(AgentMessage::tool(call.id, content));
            }
        }
        Err(AgentError::Limit(format!(
            "more than {} model steps in one run",
            self.limits.max_model_steps
        )))
    }

    async fn check_permission(
        &self,
        call: &ToolCall,
        cancel: &CancellationToken,
        pending: &PendingPermissions,
        sink: &mut EventSink,
    ) -> Result<Option<crate::ToolOutput>, AgentError> {
        match self.permission_policy.mode_for(&call.name) {
            PermissionMode::Allow => Ok(None),
            PermissionMode::Deny => Ok(Some(crate::ToolOutput::error(format!(
                "Permission denied for {}",
                call.name
            )))),
            PermissionMode::Ask => {
                let request_id = Uuid::new_v4().to_string();
                let (reply_sender, reply_receiver) = tokio::sync::oneshot::channel();
                pending.insert(request_id.clone(), reply_sender);
                if let Err(error) = sink.emit(AgentEventKind::PermissionRequested {
                    request_id: request_id.clone(),
                    tool_call: call.clone(),
                }) {
                    pending.remove(&request_id);
                    return Err(error);
                }
                let resolution = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => PermissionResolution::Cancelled,
                    reply = reply_receiver => match reply {
                        Ok(PermissionChoice::Allow) => PermissionResolution::Allowed,
                        Ok(PermissionChoice::Deny) | Err(_) => PermissionResolution::Denied,
                    },
                    _ = tokio::time::sleep(self.permission_policy.timeout()) => {
                        PermissionResolution::TimedOut
                    }
                };
                pending.remove(&request_id);
                sink.emit(AgentEventKind::PermissionResolved {
                    request_id,
                    tool_call_id: call.id.clone(),
                    resolution,
                })?;
                match resolution {
                    PermissionResolution::Allowed => Ok(None),
                    PermissionResolution::Denied => Ok(Some(crate::ToolOutput::error(format!(
                        "Permission denied for {}",
                        call.name
                    )))),
                    PermissionResolution::TimedOut => Ok(Some(crate::ToolOutput::error(format!(
                        "Permission request timed out for {}",
                        call.name
                    )))),
                    PermissionResolution::Cancelled => Err(AgentError::Cancelled),
                }
            }
        }
    }

    async fn model_step(
        &self,
        messages: &[AgentMessage],
        definitions: &[ToolDefinition],
        cancel: CancellationToken,
        sink: &mut EventSink,
    ) -> Result<crate::ModelOutput, AgentError> {
        let (delta_sender, mut delta_receiver) = tokio::sync::mpsc::unbounded_channel();
        let future =
            self.model
                .stream_complete(messages, definitions, cancel.clone(), delta_sender);
        tokio::pin!(future);
        let output = loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                result = &mut future => break result?,
                Some(content) = delta_receiver.recv() => {
                    sink.emit(AgentEventKind::AssistantDelta { content })?;
                }
            }
        };
        while let Ok(content) = delta_receiver.try_recv() {
            sink.emit(AgentEventKind::AssistantDelta { content })?;
        }
        Ok(output)
    }
}

fn canonical_workspace(path: &Path) -> Result<PathBuf, AgentError> {
    let path = path
        .canonicalize()
        .map_err(|error| AgentError::InvalidRequest(format!("workspace: {error}")))?;
    if !path.is_dir() {
        return Err(AgentError::InvalidRequest(
            "workspace is not a directory".into(),
        ));
    }
    Ok(path)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}\n[tool output truncated]")
    } else {
        prefix
    }
}

struct EventSink {
    session_id: String,
    sequence: u64,
    sender: mpsc::Sender<AgentEvent>,
}

impl EventSink {
    fn emit(&mut self, kind: AgentEventKind) -> Result<(), AgentError> {
        self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
            AgentError::Limit("event sequence exceeded the supported range".into())
        })?;
        self.sender
            .send(AgentEvent::new(&self.session_id, self.sequence, kind))
            .map_err(|_| AgentError::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelOutput, ToolOutput};
    use async_trait::async_trait;
    use openide_core::{MessageRole, ToolCall};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    struct ScriptedModel {
        outputs: Mutex<Vec<ModelOutput>>,
        seen: Mutex<Vec<Vec<AgentMessage>>>,
    }

    #[async_trait]
    impl ModelBackend for ScriptedModel {
        fn model_id(&self) -> &str {
            "scripted"
        }

        async fn complete(
            &self,
            messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            self.seen.lock().unwrap().push(messages.to_vec());
            Ok(self.outputs.lock().unwrap().remove(0))
        }
    }

    struct EchoTool;

    #[async_trait]
    impl ToolExecutor for EchoTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo text".into(),
                parameters: json!({"type":"object"}),
            }]
        }

        async fn execute(
            &self,
            call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            Ok(ToolOutput::success(
                call.arguments["text"].as_str().unwrap(),
            ))
        }
    }

    struct CountingTool {
        executions: AtomicUsize,
    }

    #[async_trait]
    impl ToolExecutor for CountingTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo text".into(),
                parameters: json!({"type":"object"}),
            }]
        }

        async fn execute(
            &self,
            call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::success(
                call.arguments["text"].as_str().unwrap(),
            ))
        }
    }

    fn model_with_tool_call() -> Arc<ScriptedModel> {
        Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_permission".into(),
                        name: "echo".into(),
                        arguments: json!({"text":"hello"}),
                    }],
                },
                ModelOutput {
                    content: "Done".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        })
    }

    fn receive_until_terminal(handle: &AgentRunHandle) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        loop {
            let event = handle
                .events
                .recv_timeout(Duration::from_secs(5))
                .expect("terminal event");
            let terminal = matches!(
                event.kind,
                AgentEventKind::TurnCompleted { .. }
                    | AgentEventKind::TurnFailed { .. }
                    | AgentEventKind::TurnCancelled
            );
            events.push(event);
            if terminal {
                return events;
            }
        }
    }

    fn wait_for_permission(handle: &AgentRunHandle) -> String {
        loop {
            let event = handle
                .events
                .recv_timeout(Duration::from_secs(5))
                .expect("permission request");
            if let AgentEventKind::PermissionRequested {
                request_id,
                tool_call,
            } = event.kind
            {
                assert_eq!(tool_call.id, "call_permission");
                assert_eq!(tool_call.arguments, json!({"text":"hello"}));
                return request_id;
            }
        }
    }

    fn request(workspace: PathBuf) -> AgentRunRequest {
        AgentRunRequest {
            session_id: Some("session_1".into()),
            sequence_start: 7,
            workspace,
            history: vec![],
            prompt: "Say hello".into(),
        }
    }

    #[test]
    fn active_skills_enter_system_context_without_rewriting_user_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![ModelOutput {
                content: "Done".into(),
                tool_calls: vec![],
            }]),
            seen: Mutex::new(vec![]),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool))
            .with_system_instructions("Skill: inspect dependency changes before editing");
        let events = receive_until_terminal(&runner.start(request(dir.path().to_path_buf())));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::TurnStarted { prompt } if prompt == "Say hello"
        )));
        let seen = model.seen.lock().unwrap();
        let messages = &seen[0];
        assert_eq!(messages[0].role, MessageRole::System);
        assert!(messages[0]
            .content
            .contains("Skill: inspect dependency changes"));
        assert_eq!(messages.last().unwrap().role, MessageRole::User);
        assert_eq!(messages.last().unwrap().content, "Say hello");
    }

    #[test]
    fn runs_model_tool_model_cycle_and_preserves_history() {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "echo".into(),
                        arguments: json!({"text":"hello"}),
                    }],
                },
                ModelOutput {
                    content: "Hello!".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let events: Vec<_> = handle.events.into_iter().collect();
        assert_eq!(events.first().unwrap().sequence, 8);
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { steps: 2, .. }
        ));
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].last().unwrap().role, MessageRole::Tool);
        assert_eq!(seen[1].last().unwrap().content, "hello");
    }

    #[test]
    fn compacts_tool_result_for_next_model_request_but_preserves_event() {
        struct LargeTool;

        #[async_trait]
        impl ToolExecutor for LargeTool {
            fn definitions(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition {
                    name: "large_output".into(),
                    description: "Produce a large result".into(),
                    parameters: json!({"type":"object"}),
                }]
            }

            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                Ok(ToolOutput::success("x".repeat(4_000)))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "large-call".into(),
                        name: "large_output".into(),
                        arguments: json!({}),
                    }],
                },
                ModelOutput {
                    content: "Done".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(LargeTool)).with_context_policy(
            ContextPolicy {
                max_request_chars: 1_500,
                keep_recent_tool_results: 0,
            },
        );
        let events: Vec<_> = runner
            .start(request(dir.path().to_path_buf()))
            .events
            .into_iter()
            .collect();
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { output, .. } if output.len() == 4_000
        )));
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1][1].content, "Say hello");
        assert_eq!(seen[1][2].tool_calls[0].id, "large-call");
        assert_eq!(seen[1][3].tool_call_id.as_deref(), Some("large-call"));
        assert_eq!(
            seen[1][3].content,
            "[Earlier tool result omitted from model context]"
        );
    }

    #[test]
    fn reports_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        struct WaitModel;
        #[async_trait]
        impl ModelBackend for WaitModel {
            fn model_id(&self) -> &str {
                "wait"
            }

            async fn complete(
                &self,
                _messages: &[AgentMessage],
                _tools: &[ToolDefinition],
                cancel: CancellationToken,
            ) -> Result<ModelOutput, AgentError> {
                cancel.cancelled().await;
                Err(AgentError::Cancelled)
            }
        }
        let runner = AgentRunner::new(Arc::new(WaitModel), Arc::new(EchoTool));
        let handle = runner.start(request(dir.path().to_path_buf()));
        handle.cancel();
        let events: Vec<_> = handle.events.into_iter().collect();
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCancelled
        ));
    }

    #[test]
    fn approval_is_bound_to_the_pending_request_before_tool_execution() {
        let dir = tempfile::tempdir().unwrap();
        let model = model_with_tool_call();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model.clone(), tool.clone())
            .with_permission_policy(PermissionPolicy::new().with_tool("echo", PermissionMode::Ask));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let id = wait_for_permission(&handle);
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
        assert!(!handle.resolve_permission("wrong-id", PermissionChoice::Allow));
        assert!(handle.resolve_permission(&id, PermissionChoice::Allow));
        assert!(!handle.resolve_permission(&id, PermissionChoice::Allow));
        let events = receive_until_terminal(&handle);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PermissionResolved { request_id, tool_call_id, resolution: PermissionResolution::Allowed }
                if request_id == &id && tool_call_id == "call_permission"
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { is_error: false, output, .. } if output == "hello"
        )));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn denial_returns_a_tool_error_without_executing_it() {
        let dir = tempfile::tempdir().unwrap();
        let model = model_with_tool_call();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model.clone(), tool.clone())
            .with_permission_policy(PermissionPolicy::new().with_tool("echo", PermissionMode::Ask));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let id = wait_for_permission(&handle);
        assert!(handle.resolve_permission(&id, PermissionChoice::Deny));
        let events = receive_until_terminal(&handle);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PermissionResolved {
                resolution: PermissionResolution::Denied,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { is_error: true, output, .. }
                if output.contains("Permission denied")
        )));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
        assert!(model.seen.lock().unwrap()[1]
            .last()
            .unwrap()
            .content
            .contains("Permission denied"));
    }

    #[test]
    fn deny_policy_blocks_a_tool_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone()).with_permission_policy(
            PermissionPolicy::new().with_tool("echo", PermissionMode::Deny),
        );
        let handle = runner.start(request(dir.path().to_path_buf()));
        let events = receive_until_terminal(&handle);
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::PermissionRequested { .. })));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { is_error: true, output, .. }
                if output.contains("Permission denied")
        )));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unhandled_permission_times_out_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone()).with_permission_policy(
            PermissionPolicy::new()
                .with_tool("echo", PermissionMode::Ask)
                .with_timeout(Duration::from_millis(20)),
        );
        let handle = runner.start(request(dir.path().to_path_buf()));
        let id = wait_for_permission(&handle);
        let events = receive_until_terminal(&handle);
        assert!(!handle.resolve_permission(&id, PermissionChoice::Allow));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PermissionResolved {
                resolution: PermissionResolution::TimedOut,
                ..
            }
        )));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancellation_while_waiting_for_permission_stops_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone())
            .with_permission_policy(PermissionPolicy::new().with_tool("echo", PermissionMode::Ask));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let id = wait_for_permission(&handle);
        handle.cancel();
        let events = receive_until_terminal(&handle);
        assert!(!handle.resolve_permission(&id, PermissionChoice::Allow));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PermissionResolved {
                resolution: PermissionResolution::Cancelled,
                ..
            }
        )));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCancelled
        ));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn truncates_utf8_by_characters() {
        assert_eq!(truncate_chars("áéí", 2), "áé\n[tool output truncated]");
    }
}
