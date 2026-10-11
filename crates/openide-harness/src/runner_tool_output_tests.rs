#[test]
fn mcp_tool_media_survives_result_truncation_restart_and_interrupted_replay() {
    struct MediaTool;
    #[async_trait]
    impl ToolExecutor for MediaTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "mcp__browser__snapshot".into(),
                description: "Capture image".into(),
                parameters: json!({"type":"object"}),
            }]
        }
        async fn execute(&self, _: &ToolCall, _: &ToolContext) -> Result<ToolOutput, AgentError> {
            Ok(ToolOutput::success(
                crate::tool_media::test_mcp_image_result().0,
            ))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
    let request = journaled_request(&store, dir.path());
    let session_id = request.session_id.clone().unwrap();
    let lease = store.try_acquire_native_lease(&session_id).unwrap();
    let model = Arc::new(ScriptedModel {
        outputs: Mutex::new(vec![
            ModelOutput {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "image-call".into(),
                    name: "mcp__browser__snapshot".into(),
                    arguments: json!({}),
                }],
            },
            ModelOutput {
                content: "Complete".into(),
                tool_calls: vec![],
            },
        ]),
        seen: Mutex::new(vec![]),
    });
    #[cfg(windows)]
    let hook_script = "[Console]::Out.Write('{\"decision\":\"allow\",\"additional_context\":\"Image hook context\"}')";
    #[cfg(unix)]
    let hook_script =
        "printf '%s' '{\"decision\":\"allow\",\"additional_context\":\"Image hook context\"}'";
    let hooks = [HookPhase::PreToolUse, HookPhase::PostToolUse]
        .into_iter()
        .enumerate()
        .map(|(index, phase)| {
            let mut config = command_hook_config(dir.path(), phase, hook_script, 5_000);
            config.id = format!("media-hook-{index}");
            config.tool_name = "mcp__browser__snapshot".into();
            config
        })
        .collect();
    let runner = AgentRunner::new(model.clone(), Arc::new(MediaTool))
        .with_command_hooks(Arc::new(CommandHooks::new(hooks).unwrap()))
        .with_native_runtime_identity("test", "https://test.example/v1")
        .with_permission_policy(PermissionPolicy::new().with_default_mode(PermissionMode::Allow))
        .with_event_journal(store.clone());
    let handle = runner.start_with_lease(request, lease);
    let observed = receive_until_terminal(&handle);
    assert!(matches!(
        observed.last().unwrap().kind,
        AgentEventKind::TurnCompleted { .. }
    ));
    let saved = SessionStore::open(dir.path().join("sessions"))
        .unwrap()
        .load(&session_id)
        .unwrap()
        .unwrap()
        .events;
    let at = saved
        .iter()
        .position(|event| matches!(event.kind, AgentEventKind::ToolResultMedia { .. }))
        .unwrap();
    let AgentEventKind::ToolResultMedia {
        tool_call_id,
        images,
        text,
        notices,
    } = &saved[at].kind
    else {
        unreachable!()
    };
    assert_eq!(tool_call_id, "image-call");
    assert_eq!(
        images[0].verified_bytes().unwrap(),
        crate::tool_media::test_mcp_image_result().1
    );
    assert_eq!(text, "Captura recibida");
    assert!(notices.is_empty());
    assert!(observed.contains(&saved[at]));
    assert_eq!(
        saved[..at]
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::HookFinished { .. }))
            .count(),
        2
    );
    let AgentEventKind::ToolFinished { output, .. } = &saved[at + 1].kind else {
        panic!("media must precede ToolFinished")
    };
    // The image went to the user in the event before; the model reads a
    // note in its place, and nothing is cut off.
    assert!(output.contains("[image omitted (image/png") && !output.contains("[tool output truncated]"));
    let compatibility = CheckpointCompatibility {
        provider: "test".into(),
        model: "scripted".into(),
        context_format_key: "test-v1".into(),
    };
    let replay = replay_native(&session_id, &saved, None, &compatibility).unwrap();
    let tool_messages = replay
        .history
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 1);
    assert_eq!(&tool_messages[0].content, output);
    assert!(tool_messages[0].images.is_empty());
    assert!(model.seen.lock().unwrap()[1]
        .iter()
        .all(|message| message.images.is_empty()));

    // A crash/cancel after persisting media must keep the effect uncertain,
    // without promoting the observation into a model-facing tool result.
    let mut interrupted = saved[..=at].to_vec();
    interrupted.push(AgentEvent::new(
        &session_id,
        saved[at].sequence + 1,
        AgentEventKind::TurnCancelled,
    ));
    let replay = replay_native(&session_id, &interrupted, None, &compatibility).unwrap();
    assert!(matches!(
        replay.recovery,
        ReplayRecovery::ToolOutcomeUncertain { .. }
    ));
    assert!(replay
        .history
        .iter()
        .all(|message| message.role != MessageRole::Tool));
    for kind in [
        saved[at].kind.clone(),
        AgentEventKind::ToolOutputDelta {
            tool_call_id: "image-call".into(),
            stream: openide_core::OutputStream::Stdout,
            text: "late".into(),
        },
    ] {
        let mut late = interrupted.clone();
        late.push(AgentEvent::new(&session_id, saved[at].sequence + 2, kind));
        assert!(replay_native(&session_id, &late, None, &compatibility).is_err());
    }
    let mut duplicate = saved[..=at].to_vec();
    duplicate.push(AgentEvent::new(
        &session_id,
        saved[at].sequence + 1,
        saved[at].kind.clone(),
    ));
    assert!(replay_native(&session_id, &duplicate, None, &compatibility).is_err());
    for invalid in [
        AgentEventKind::ToolResultMedia {
            tool_call_id: "wrong-call".into(),
            images: images.clone(),
            text: text.clone(),
            notices: vec![],
        },
        AgentEventKind::ToolResultMedia {
            tool_call_id: tool_call_id.clone(),
            images: images.clone(),
            text: "x".repeat(4097),
            notices: vec![],
        },
        {
            let mut images = images.clone();
            images[0].sha256 = "0".repeat(64);
            AgentEventKind::ToolResultMedia {
                tool_call_id: tool_call_id.clone(),
                images,
                text: String::new(),
                notices: vec![],
            }
        },
    ] {
        let mut malformed = saved[..at].to_vec();
        malformed.push(AgentEvent::new(&session_id, saved[at].sequence, invalid));
        assert!(replay_native(&session_id, &malformed, None, &compatibility).is_err());
    }
}

#[cfg(unix)]
#[test]
fn command_tool_output_is_durable_before_exit_and_survives_error_or_cancellation() {
    for outcome in ["success", "error", "cancel"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let script = format!(
            "printf 'Caf\\303'; sleep 0.02; printf '\\251 \\360\\237\\246\\200\\n'; printf 'diagnostic\\n' >&2; while [ ! -f release ]; do sleep 0.01; done; printf 'finished\\n'; exit {}",
            if outcome == "error" { 3 } else { 0 }
        );
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "command-live".into(),
                        name: "run_command".into(),
                        arguments: json!({"command":"sh","args":["-c",script]}),
                    }],
                },
                ModelOutput {
                    content: "Complete".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        });
        let tools = Arc::new(
            crate::LocalTools::new()
                .with_write_access(true)
                .with_command_isolation(crate::CommandIsolation::HostUnrestricted),
        );
        let runner = AgentRunner::new(model.clone(), tools)
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_permission_policy(
                PermissionPolicy::new().with_default_mode(PermissionMode::Allow),
            )
            .with_event_journal(store.clone());
        let handle = runner.start_with_lease(request, lease);
        let mut seen = Vec::new();
        let mut stdout = String::new();
        let mut stderr = String::new();
        loop {
            let event = handle.events.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(
                !matches!(
                    event.kind,
                    AgentEventKind::ToolFinished { .. } | AgentEventKind::TurnFailed { .. }
                ),
                "command finished before release: {:?}",
                event.kind
            );
            if let AgentEventKind::ToolOutputDelta {
                tool_call_id,
                stream,
                text,
            } = &event.kind
            {
                assert_eq!(tool_call_id, "command-live");
                // The observer only sees data after the journal write succeeded.
                let stored = store.load(&session_id).unwrap().unwrap();
                assert!(stored.events.iter().any(|saved| saved == &event));
                match stream {
                    openide_core::OutputStream::Stdout => stdout.push_str(text),
                    openide_core::OutputStream::Stderr => stderr.push_str(text),
                }
            }
            seen.push(event);
            if stdout.contains("Café 🦀") && stderr.contains("diagnostic") {
                break;
            }
        }
        assert!(!stdout.contains('\u{fffd}'));
        if outcome == "cancel" {
            handle.cancel();
        } else {
            std::fs::write(dir.path().join("release"), "go").unwrap();
        }
        seen.extend(receive_until_terminal(&handle));
        assert!(matches!(
            handle.events.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        let stored = SessionStore::open(dir.path().join("sessions"))
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored
                .events
                .iter()
                .filter(|e| matches!(e.kind, AgentEventKind::ToolOutputDelta { .. }))
                .count(),
            seen.iter()
                .filter(|e| matches!(e.kind, AgentEventKind::ToolOutputDelta { .. }))
                .count()
        );
        let replay = replay_native(
            &session_id,
            &stored.events,
            None,
            &CheckpointCompatibility {
                provider: "test".into(),
                model: "scripted".into(),
                context_format_key: "test-v1".into(),
            },
        )
        .unwrap();
        let results = replay
            .history
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .collect::<Vec<_>>();
        if outcome == "cancel" {
            assert!(matches!(
                seen.last().unwrap().kind,
                AgentEventKind::TurnCancelled
            ));
            assert!(matches!(
                replay.recovery,
                openide_core::ReplayRecovery::ToolOutcomeUncertain { .. }
            ));
            assert!(results.is_empty());
        } else {
            assert!(matches!(
                seen.last().unwrap().kind,
                AgentEventKind::TurnCompleted { .. }
            ));
            assert_eq!(
                results.len(),
                1,
                "partial output never adds model-history messages"
            );
            assert!(results[0].content.contains("finished"));
            assert!(seen.iter().any(|e| matches!(&e.kind, AgentEventKind::ToolFinished { is_error, .. } if *is_error == (outcome == "error"))));
            let seen_requests = model.seen.lock().unwrap();
            assert_eq!(
                seen_requests[1]
                    .iter()
                    .filter(|m| m.role == MessageRole::Tool)
                    .count(),
                1
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn tool_output_journal_failure_stops_execution_without_post_hooks_or_finished_result() {
    struct StreamingTool;
    #[async_trait]
    impl ToolExecutor for StreamingTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            EchoTool.definitions()
        }
        async fn execute(&self, _: &ToolCall, _: &ToolContext) -> Result<ToolOutput, AgentError> {
            unreachable!()
        }
        async fn execute_streaming(
            &self,
            _: &ToolCall,
            _: &ToolContext,
            output: crate::ToolOutputSender,
        ) -> Result<ToolOutput, AgentError> {
            output
                .send(
                    openide_core::OutputStream::Stdout,
                    "side effect may have happened",
                )
                .await?;
            std::future::pending().await
        }
    }
    struct RejectOutput(Arc<SessionStore>);
    impl NativeEventJournal for RejectOutput {
        fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
            if matches!(event.kind, AgentEventKind::ToolOutputDelta { .. }) {
                return Err(StoreError::Io(std::io::Error::other(
                    "injected output append failure",
                )));
            }
            self.0.append_native_event(event)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
    let request = journaled_request(&store, dir.path());
    let session_id = request.session_id.clone().unwrap();
    let lease = store.try_acquire_native_lease(&session_id).unwrap();
    let hooks = Arc::new(
        CommandHooks::new(vec![command_hook_config(
            dir.path(),
            HookPhase::PostToolUse,
            "touch forbidden-post-hook; printf '%s' '{\"decision\":\"allow\"}'",
            5_000,
        )])
        .unwrap(),
    );
    let runner = AgentRunner::new(model_with_tool_call(), Arc::new(StreamingTool))
        .with_command_hooks(hooks)
        .with_native_runtime_identity("test", "https://test.example/v1")
        .with_event_journal(Arc::new(RejectOutput(store.clone())));
    let handle = runner.start_with_lease(request, lease);
    loop {
        match handle.events.recv_timeout(Duration::from_secs(5)) {
            Ok(event) => assert!(!matches!(
                event.kind,
                AgentEventKind::ToolOutputDelta { .. }
                    | AgentEventKind::ToolFinished { .. }
                    | AgentEventKind::HookStarted { .. }
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(error) => panic!("failed output delivery did not stop execution: {error}"),
        }
    }
    assert!(handle
        .journal_error()
        .unwrap()
        .contains("injected output append failure"));
    assert!(!dir.path().join("forbidden-post-hook").exists());
    let saved = store.load(&session_id).unwrap().unwrap();
    assert!(matches!(
        saved.events.last().unwrap().kind,
        AgentEventKind::ToolStarted { .. }
    ));
    let replay = replay_native(
        &session_id,
        &saved.events,
        None,
        &CheckpointCompatibility {
            provider: "test".into(),
            model: "scripted".into(),
            context_format_key: "test-v1".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        replay.recovery,
        openide_core::ReplayRecovery::ToolOutcomeUncertain { .. }
    ));
}
