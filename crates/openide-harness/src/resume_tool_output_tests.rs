#[tokio::test]
async fn resumed_mcp_tool_media_is_durable_before_truncation_and_observer_failure_keeps_uncertainty(
) {
    struct MediaObserver {
        store: Arc<SessionStore>,
        fail: bool,
    }
    #[async_trait]
    impl ResumeEventObserver for MediaObserver {
        fn on_persisted_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
            if matches!(event.kind, AgentEventKind::ToolResultMedia { .. }) {
                assert!(self
                    .store
                    .load(&event.session_id)
                    .unwrap()
                    .unwrap()
                    .events
                    .contains(event));
                if self.fail {
                    return Err(AgentError::Runtime(
                        "injected media observer failure".into(),
                    ));
                }
            }
            Ok(())
        }
    }
    for outcome in ["success", "error", "observer_failure"] {
        let fixture = Fixture::new(vec![done()]);
        #[cfg(windows)]
        let hook_script = "[Console]::Out.Write('{\"decision\":\"allow\",\"additional_context\":\"Image hook context\"}')";
        #[cfg(unix)]
        let hook_script =
            "printf '%s' '{\"decision\":\"allow\",\"additional_context\":\"Image hook context\"}'";
        let base_hooks = recovery_hook(fixture._dir.path(), hook_script);
        let mut config = base_hooks
            .for_call(HookPhase::PreToolUse, "read_file")
            .next()
            .unwrap()
            .clone();
        config.tool_name = "mcp__browser__snapshot".into();
        config.phase = HookPhase::PostToolUse;
        let hooks = Arc::new(CommandHooks::new(vec![config]).unwrap());
        let mut mcp_definition = definition();
        mcp_definition.name = "mcp__browser__snapshot".into();
        let catalog = ToolCatalog::new([mcp_definition]).unwrap();
        let mut mcp_call = call("mcp-image-call");
        mcp_call.name = "mcp__browser__snapshot".into();
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: catalog.digest_hex(),
        });
        fixture.append(AgentEventKind::McpRuntimeCommitted {
            version: MCP_RUNTIME_IDENTITY_VERSION,
            digest: "a".repeat(64),
        });
        fixture.append(AgentEventKind::HooksCommitted {
            version: HOOK_CONFIG_IDENTITY_VERSION,
            digest: hooks.digest_hex().unwrap(),
        });
        let workspace = fixture
            .sessions
            .load(&fixture.session_id)
            .unwrap()
            .unwrap()
            .session
            .workspace;
        fixture.append(AgentEventKind::ModelContextCommitted {
            version: MODEL_CONTEXT_VERSION,
            digest: crate::native_model_context_digest(
                "openai",
                "https://test.example/v1",
                "gpt-example",
                TurnMode::Build,
                &workspace,
                &catalog,
                "",
                [7; 32],
            )
            .unwrap(),
        });
        fixture.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![mcp_call]),
            response_id: None,
            usage: None,
        });
        let (full_output, image_bytes) = crate::tool_media::test_mcp_image_result();
        *fixture.tools.result.lock().unwrap() = Some(Ok(ToolOutput {
            content: full_output.clone(),
            is_error: outcome == "error",
        }));
        let engine = ResumeEngine::new(
            fixture.sessions.clone(),
            fixture.checkpoints.clone(),
            compatibility(),
            fixture.model.clone(),
            fixture.tools.clone(),
            catalog,
            PermissionPolicy::new().with_default_mode(PermissionMode::Allow),
            budget(),
        )
        .with_native_runtime_identity("openai", "https://test.example/v1")
        .with_command_hooks(hooks)
        .with_mcp_runtime_identity_digest(Some("a".repeat(64)))
        .with_event_observer(Arc::new(MediaObserver {
            store: fixture.sessions.clone(),
            fail: outcome == "observer_failure",
        }));
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let result = engine.resume_step(&lease, CancellationToken::new()).await;
        assert_eq!(result.is_err(), outcome == "observer_failure");
        let saved = SessionStore::open(fixture._dir.path())
            .unwrap()
            .load(&fixture.session_id)
            .unwrap()
            .unwrap()
            .events;
        let at = saved
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ToolResultMedia { .. }))
            .expect("media before truncation");
        let AgentEventKind::ToolResultMedia { images, text, .. } = &saved[at].kind else {
            unreachable!()
        };
        assert_eq!(images[0].verified_bytes().unwrap(), image_bytes);
        assert_eq!(text, "Captura recibida");
        assert!(matches!(
            saved[at - 1].kind,
            AgentEventKind::HookFinished { .. }
        ));
        let replay = replay_native(&fixture.session_id, &saved, None, &compatibility()).unwrap();
        if outcome == "observer_failure" {
            assert!(!saved
                .iter()
                .any(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. })));
            assert!(matches!(
                replay.recovery,
                ReplayRecovery::ToolOutcomeUncertain { .. }
            ));
            assert!(replay
                .history
                .iter()
                .all(|message| message.role != openide_core::MessageRole::Tool));
        } else {
            let AgentEventKind::ToolFinished {
                output, is_error, ..
            } = &saved[at + 1].kind
            else {
                panic!("media must precede ToolFinished")
            };
            assert_eq!(*is_error, outcome == "error");
            assert_eq!(
                output,
                &full_output.chars().take(16_000).collect::<String>()
            );
            let results = replay
                .history
                .iter()
                .filter(|message| message.role == openide_core::MessageRole::Tool)
                .collect::<Vec<_>>();
            assert_eq!(results.len(), 1);
            assert_eq!(&results[0].content, output);
            assert!(results[0].images.is_empty());
        }
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
    }
}

struct StreamingRecoveryTool {
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    outcome: &'static str,
}

#[async_trait]
impl ToolExecutor for StreamingRecoveryTool {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![definition()]
    }
    async fn execute(&self, _: &ToolCall, _: &ToolContext) -> Result<ToolOutput, AgentError> {
        panic!("recovery must call execute_streaming")
    }
    async fn execute_streaming(
        &self,
        _: &ToolCall,
        context: &ToolContext,
        output: crate::ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        output
            .send(openide_core::OutputStream::Stdout, "Café 🦀")
            .await?;
        let release = self.release.lock().unwrap().take().unwrap();
        release.await.unwrap();
        output
            .send(openide_core::OutputStream::Stderr, "diagnostic")
            .await?;
        match self.outcome {
            "success" => Ok(ToolOutput::success("authoritative result")),
            "error" => Err(AgentError::EffectUncertain(
                "lost completion after output".into(),
            )),
            _ => {
                context.cancel.cancel();
                std::future::pending().await
            }
        }
    }
}

struct ToolProgressObserver {
    store: Arc<SessionStore>,
    first: tokio::sync::mpsc::UnboundedSender<()>,
}

#[async_trait]
impl ResumeEventObserver for ToolProgressObserver {
    fn on_persisted_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        if matches!(event.kind, AgentEventKind::ToolOutputDelta { .. }) {
            assert!(self
                .store
                .load(&event.session_id)
                .unwrap()
                .unwrap()
                .events
                .contains(event));
            self.first.send(()).unwrap();
        }
        Ok(())
    }
}

#[tokio::test]
async fn recovery_tool_output_is_durable_before_result_and_keeps_accepted_error_cancel_prefix() {
    for outcome in ["success", "error", "cancel"] {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let (release, receive_release) = tokio::sync::oneshot::channel();
        let (first, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let mut engine = fixture
            .engine(PermissionMode::Allow)
            .with_event_observer(Arc::new(ToolProgressObserver {
                store: fixture.sessions.clone(),
                first,
            }));
        engine.tools = Arc::new(StreamingRecoveryTool {
            release: Mutex::new(Some(receive_release)),
            outcome,
        });
        let cancel = CancellationToken::new();
        let future = engine.resume_step(&lease, cancel);
        tokio::pin!(future);
        tokio::select! {
            result = &mut future => panic!("completed before live output: {result:?}"),
            seen = observed.recv() => assert!(seen.is_some()),
        }
        assert!(!fixture
            .events()
            .iter()
            .any(|e| matches!(e.kind, AgentEventKind::ToolFinished { .. })));
        release.send(()).unwrap();
        future.await.unwrap();
        let saved = fixture.events();
        let partial = saved
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::ToolOutputDelta {
                    tool_call_id, text, ..
                } => {
                    assert_eq!(tool_call_id, "call-1");
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(partial, ["Café 🦀", "diagnostic"]);
        let reopened = SessionStore::open(fixture._dir.path())
            .unwrap()
            .load(&fixture.session_id)
            .unwrap()
            .unwrap();
        assert_eq!(saved, reopened.events);
        let replay = replay_native(&fixture.session_id, &saved, None, &compatibility()).unwrap();
        if outcome == "success" {
            let results = replay
                .history
                .iter()
                .filter(|m| m.role == openide_core::MessageRole::Tool)
                .collect::<Vec<_>>();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].content, "authoritative result");
        } else {
            assert!(matches!(
                replay.recovery,
                ReplayRecovery::ToolOutcomeUncertain { .. }
            ));
            assert!(!saved
                .iter()
                .any(|e| matches!(e.kind, AgentEventKind::ToolFinished { .. })));
        }
        // Wrong-call and post-terminal observations must never acquire authority.
        let mut malformed = saved.clone();
        let last = malformed.last().unwrap().sequence;
        malformed.push(AgentEvent::new(
            &fixture.session_id,
            last + 1,
            AgentEventKind::ToolOutputDelta {
                tool_call_id: "foreign-call".into(),
                stream: openide_core::OutputStream::Stdout,
                text: "injected".into(),
            },
        ));
        assert!(replay_native(&fixture.session_id, &malformed, None, &compatibility()).is_err());
    }
}
