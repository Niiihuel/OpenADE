//! Bounded bridge acceptance uses a real child journal and an in-process
//! typed backend. Barriers hold the child live while the host is inspected.

use super::*;
use async_trait::async_trait;
use openide_core::ModelStreamEvent;
use openide_harness::{AgentError, ModelCompletion, ModelEventSender, ModelOutput, ToolDefinition};
use openide_ui::Point;
use std::sync::atomic::AtomicBool;
use tokio::sync::Notify;

const MODEL: &str = "fixture-child-stream";
const FLOOD_PAIRS: usize = 300;
const TAIL: &str = "VISIBLE_DURABLE_TAIL";

struct HeldChildModel {
    initial_sent: AtomicBool,
    flood: Notify,
    complete: Notify,
}

impl HeldChildModel {
    fn new() -> Self {
        Self {
            initial_sent: AtomicBool::new(false),
            flood: Notify::new(),
            complete: Notify::new(),
        }
    }

    fn final_text() -> String {
        format!("AC{}{TAIL}", "x".repeat(FLOOD_PAIRS))
    }
}

#[async_trait]
impl ModelBackend for HeldChildModel {
    fn model_id(&self) -> &str {
        MODEL
    }

    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([77; 32])
    }

    async fn complete(
        &self,
        _messages: &[AgentMessage],
        _tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        panic!("fixture must use the bounded typed streaming entrypoint")
    }

    async fn stream_events(
        &self,
        _messages: &[AgentMessage],
        _tools: &[ToolDefinition],
        cancel: CancellationToken,
        events: ModelEventSender,
    ) -> Result<ModelCompletion, AgentError> {
        events
            .send(ModelStreamEvent::TextDelta {
                content: "A".into(),
            })
            .await?;
        events
            .send(ModelStreamEvent::ReasoningDelta {
                item_id: "reasoning".into(),
                content: "Initial reason".into(),
            })
            .await?;
        events
            .send(ModelStreamEvent::TextDelta {
                content: "C".into(),
            })
            .await?;
        self.initial_sent.store(true, Ordering::SeqCst);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            _ = self.flood.notified() => {}
        }
        // Interleave event kinds so text coalescing cannot turn this into a
        // single bridge notification. This exceeds its 256-event capacity.
        for _ in 0..FLOOD_PAIRS {
            events
                .send(ModelStreamEvent::ReasoningDelta {
                    item_id: "reasoning".into(),
                    content: "Reason".into(),
                })
                .await?;
            events
                .send(ModelStreamEvent::TextDelta {
                    content: "x".into(),
                })
                .await?;
        }
        events
            .send(ModelStreamEvent::TextDelta {
                content: TAIL.into(),
            })
            .await?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            _ = self.complete.notified() => {}
        }
        Ok(ModelOutput {
            content: Self::final_text(),
            tool_calls: vec![],
        }
        .into())
    }
}

struct ChildFixture {
    _root: tempfile::TempDir,
    app: AgentsWindow,
    sessions: Arc<SessionStore>,
    model: Arc<HeldChildModel>,
    child_id: String,
    cancellation: CancellationToken,
    running: Option<
        tokio::task::JoinHandle<
            Result<openide_harness::SubagentTurnResult, openide_harness::SubagentWorkerError>,
        >,
    >,
}

impl Drop for ChildFixture {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl ChildFixture {
    async fn start() -> Self {
        Self::start_with_prompt("Inspect").await
    }

    async fn start_with_prompt(prompt: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let app = AgentsWindow::with_data_root(root.path().join("data"));
        let sessions = app.store.clone().unwrap();
        let parent = sessions.create(&workspace, "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator = Arc::new(
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap(),
        );
        let model = Arc::new(HeldChildModel::new());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new().with_write_access(false)),
            "fixture",
            "https://fixture.invalid/v1",
        )
        .with_system_instructions("In-process child streaming fixture; no external resources")
        .with_event_bridge(app.subagent_bridge.clone());
        let child = worker
            .prepare_child(&lease, "child-stream-spawn", prompt, TurnMode::Build)
            .unwrap();
        let child_id = child.session.id.clone();
        let control = coordinator.issue_control(&lease).unwrap();
        drop(lease);
        let cancellation = CancellationToken::new();
        let running_id = child_id.clone();
        let running_cancel = cancellation.clone();
        let prompt = prompt.to_owned();
        let running = tokio::spawn(async move {
            worker
                .run_prepared(
                    control,
                    running_id,
                    "child-stream-spawn".into(),
                    prompt,
                    TurnMode::Build,
                    running_cancel,
                )
                .await
        });
        let fixture = Self {
            _root: root,
            app,
            sessions,
            model,
            child_id,
            cancellation,
            running: Some(running),
        };
        fixture.wait_durable_text("AC").await;
        assert!(fixture.model.initial_sent.load(Ordering::SeqCst));
        assert!(fixture
            .app
            .subagent_bridge
            .active_child_ids()
            .contains(&fixture.child_id));
        fixture
    }

    async fn wait_durable_text(&self, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let stored = self.sessions.load(&self.child_id).unwrap().unwrap();
            let text = stored
                .events
                .iter()
                .filter_map(|event| match &event.kind {
                    AgentEventKind::AssistantDelta { content } => Some(content.as_str()),
                    _ => None,
                })
                .collect::<String>();
            if text == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "child did not persist its bounded event tail"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn drain_host(&mut self) {
        // The bridge is finite while the model is held at a barrier (or after
        // the worker has exited). Each host poll still observes its frame cap.
        for _ in 0..12 {
            if !self.app.poll_subagent_events() {
                return;
            }
        }
        panic!("bounded child bridge did not finish draining");
    }

    async fn finish(&mut self) {
        self.model.complete.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(20), self.running.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.status, SubagentStatus::Completed);
    }
}

fn text_rows(app: &AgentsWindow) -> Vec<(&str, bool)> {
    app.view
        .transcript
        .iter()
        .filter_map(|row| match row {
            TranscriptItem::Assistant { content, streaming } => {
                Some((content.as_str(), *streaming))
            }
            _ => None,
        })
        .collect()
}

fn assert_live(app: &AgentsWindow, child_id: &str) {
    assert!(text_rows(app)
        .last()
        .is_some_and(|(_, streaming)| *streaming));
    assert!(!app
        .view
        .transcript
        .iter()
        .any(|row| matches!(row, TranscriptItem::Notice { .. })));
    assert!(
        !app.view.session_recovery.contains_key(child_id),
        "a confirmed live child must not expose crash recovery"
    );
    assert_eq!(app.view.status, "Subagente en curso");
}

#[tokio::test]
async fn selecting_and_reconnecting_overflowed_live_child_preserves_receiving_tail() {
    let mut fixture = ChildFixture::start().await;
    fixture.app.open_session(&fixture.child_id);
    assert_eq!(text_rows(&fixture.app), [("A", false), ("C", true)]);
    assert_live(&fixture.app, &fixture.child_id);
    fixture.model.flood.notify_one();
    fixture
        .wait_durable_text(&HeldChildModel::final_text())
        .await;
    fixture.drain_host();
    let current = text_rows(&fixture.app)
        .iter()
        .map(|(content, _)| *content)
        .collect::<String>();
    assert_eq!(current, HeldChildModel::final_text());
    assert_live(&fixture.app, &fixture.child_id);
    assert!(fixture.app.view.transcript.iter().any(|row| matches!(
        row,
        TranscriptItem::Reasoning {
            status: crate::transcript::ModelStreamStatus::Receiving,
            ..
        }
    )));
    assert!(fixture
        .app
        .subagent_bridge
        .active_child_ids()
        .contains(&fixture.child_id));
    fixture.finish().await;
    fixture.drain_host();
    assert!(text_rows(&fixture.app)
        .iter()
        .all(|(_, streaming)| !streaming));
    assert!(!fixture
        .app
        .view
        .session_recovery
        .contains_key(&fixture.child_id));
}

#[tokio::test]
async fn disconnected_child_subscriber_recovers_missed_terminal_without_future_events() {
    let mut fixture = ChildFixture::start().await;
    fixture.app.open_session(&fixture.child_id);
    let selected_sequence = fixture.app.last_sequence;
    fixture.model.flood.notify_one();
    fixture
        .wait_durable_text(&HeldChildModel::final_text())
        .await;
    // Leave the bounded subscriber unpolled until its terminal and Closed
    // notifications are both dropped. No later event can prompt a repair.
    fixture.finish().await;
    assert!(fixture.app.subagent_bridge.active_child_ids().is_empty());
    let stored = fixture.sessions.load(&fixture.child_id).unwrap().unwrap();
    let durable_last_sequence = stored.events.last().unwrap().sequence;
    assert!(durable_last_sequence > selected_sequence);
    fixture.drain_host();
    assert_eq!(fixture.app.last_sequence, durable_last_sequence);
    assert_eq!(
        fixture.app.view.transcript,
        TranscriptProjection::from_events(&stored.events).items()
    );
    assert_eq!(
        text_rows(&fixture.app)
            .iter()
            .map(|(content, _)| *content)
            .collect::<String>(),
        HeldChildModel::final_text()
    );
    assert!(text_rows(&fixture.app)
        .iter()
        .all(|(_, streaming)| !streaming));
    assert!(!fixture
        .app
        .view
        .transcript
        .iter()
        .any(|row| matches!(row, TranscriptItem::Notice { .. })));
    assert!(!fixture
        .app
        .view
        .session_recovery
        .contains_key(&fixture.child_id));
    assert!(fixture
        .app
        .view
        .subagent_sessions
        .iter()
        .any(|child| child.child_session_id == fixture.child_id
            && child.status == SubagentStatus::Completed));
    assert!(
        !fixture.app.poll_subagent_events(),
        "the missed terminal must be repaired without a later producer event"
    );
}

fn scroll_history() -> String {
    (0..36)
        .map(|index| format!("Fila de historial {index}: información del hijo ñ🦀.\n\n"))
        .collect()
}

fn scroll_frame(app: &mut AgentsWindow, height: f32) -> UiFrame {
    app.sync_stream_presentation(Instant::now());
    app.layout_workspace_frame(Size::new(960.0, height))
}

fn scroll_up(app: &mut AgentsWindow, frame: &UiFrame) {
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == "transcript" && region.kind == openide_ui::HitKind::Scroll)
        .unwrap()
        .rect;
    app.tree.handle_event(UiEvent::ScrollDirect {
        position: Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0),
        delta_y: 350.0,
    });
    assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
}

fn append_child_event(app: &AgentsWindow, child_id: &str, kind: AgentEventKind) -> AgentEvent {
    let store = app.store.as_ref().unwrap();
    let stored = store.load(child_id).unwrap().unwrap();
    let sequence = stored.events.last().map_or(1, |event| event.sequence + 1);
    let event = AgentEvent::new(child_id, sequence, kind);
    store.append_event(child_id, &event).unwrap();
    event
}

// A real child journal with a controlled subscriber isolates host scrolling
// from text reveal and worker scheduling. No executor or provider is invoked.
fn journal_scroll_fixture() -> (
    tempfile::TempDir,
    AgentsWindow,
    String,
    std::sync::mpsc::Sender<SubagentBridgeEvent>,
) {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let store = app.store.as_ref().unwrap().clone();
    let parent = store.create(root.path(), "native").unwrap();
    let lease = store.try_acquire_native_lease(&parent.id).unwrap();
    let coordinator = SubagentCoordinator::new(store, SubagentLimits::default()).unwrap();
    let child = coordinator
        .create_child(
            &lease,
            "scroll-child",
            &openide_harness::SubagentSpec {
                prompt: "Inspect".into(),
                model: MODEL.into(),
                mode: TurnMode::Build,
                context_digest: "a".repeat(64),
            },
        )
        .unwrap();
    drop(lease);
    let child_id = child.session.id;
    for kind in [
        AgentEventKind::TurnStarted {
            prompt: "Inspect".into(),
        },
        AgentEventKind::AssistantMessage {
            content: scroll_history(),
        },
        AgentEventKind::ToolStarted {
            tool_call: ToolCall {
                id: "scroll-tool".into(),
                name: "run_command".into(),
                arguments: serde_json::json!({"command": "printf fixture"}),
            },
        },
    ] {
        append_child_event(&app, &child_id, kind);
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    app.subagent_events = receiver;
    app.open_session(&child_id);
    app.view.sidebar_collapsed = true;
    app.view.show_side_pane = false;
    app.reduced_motion = true;
    app.tree.set_reduced_motion(true);
    app.view
        .conversation_details
        .set_expanded("tool:scroll-tool", true);
    (root, app, child_id, sender)
}

#[test]
fn selected_child_non_text_updates_preserve_tail_or_manual_reading() {
    for update in ["output", "permission", "closed"] {
        for follow in [true, false] {
            let (_root, mut app, child_id, sender) = journal_scroll_fixture();
            let frame = scroll_frame(&mut app, 620.0);
            assert!(app.tree.is_scroll_at_end("transcript", 0.1));
            assert!(app.tree.scroll_offset("transcript") > 350.0);
            if !follow {
                scroll_up(&mut app, &frame);
                scroll_frame(&mut app, 620.0);
            }
            let previous_offset = app.tree.scroll_offset("transcript");
            let kinds = match update {
                "output" => vec![AgentEventKind::ToolOutputDelta {
                    tool_call_id: "scroll-tool".into(),
                    stream: openide_core::OutputStream::Stdout,
                    text: (0..24).map(|index| format!("Salida {index}\n")).collect(),
                }],
                "permission" => (0..8)
                    .map(|index| AgentEventKind::PermissionRequested {
                        request_id: format!("approval-{index}"),
                        tool_call: ToolCall {
                            id: format!("approval-tool-{index}"),
                            name: "write_file".into(),
                            arguments: serde_json::json!({"path": format!("file-{index}.rs"), "content": "fixture"}),
                        },
                    })
                    .collect(),
                _ => vec![AgentEventKind::TurnFailed { error: scroll_history() }],
            };
            for kind in kinds {
                let event = append_child_event(&app, &child_id, kind);
                if update != "closed" {
                    sender
                        .send(SubagentBridgeEvent::Event {
                            child_session_id: child_id.clone(),
                            event: Box::new(event),
                        })
                        .unwrap();
                }
            }
            if update == "closed" {
                // Closed alone must reload a durable terminal even when its
                // event notification was not available to this subscriber.
                sender
                    .send(SubagentBridgeEvent::Closed {
                        child_session_id: child_id.clone(),
                    })
                    .unwrap();
            }
            assert!(app.poll_subagent_events());
            assert!(
                !app.sync_stream_presentation(Instant::now()),
                "no text reveal may rescue {update}"
            );
            app.layout_workspace_frame(Size::new(960.0, 620.0));
            if follow {
                assert!(app.tree.is_scroll_at_end("transcript", 0.1), "{update}");
                assert!(
                    app.tree.scroll_offset("transcript") > previous_offset + 24.0,
                    "{update}"
                );
            } else {
                assert!(
                    (app.tree.scroll_offset("transcript") - previous_offset).abs() < 0.1,
                    "{update}"
                );
                assert!(!app.tree.is_scroll_at_end("transcript", 24.0), "{update}");
            }
        }
    }
}

#[test]
fn child_updates_do_not_inherit_a_foreign_transcript_layout_or_scroll_another_session() {
    for previous_owner_is_foreign in [true, false] {
        let (_root, mut app, child_id, sender) = journal_scroll_fixture();
        scroll_frame(&mut app, 620.0);
        if !previous_owner_is_foreign {
            app.view.selected_session = Some("another-session".into());
            // Establish the other session's own disclosure/layout state before
            // measuring its reading position. Changing owner legitimately
            // closes the previous owner's expanded tool and can clamp scroll.
            scroll_frame(&mut app, 620.0);
            app.tree.scroll_to_end("transcript");
            scroll_frame(&mut app, 620.0);
        }
        assert!(app.tree.is_scroll_at_end("transcript", 0.1));
        let previous_offset = app.tree.scroll_offset("transcript");
        let previous_transcript = app.view.transcript.clone();
        if previous_owner_is_foreign {
            app.transcript_layout_session = Some("previous-session".into());
        }
        let event = append_child_event(
            &app,
            &child_id,
            AgentEventKind::TurnFailed {
                error: scroll_history(),
            },
        );
        sender
            .send(SubagentBridgeEvent::Event {
                child_session_id: child_id,
                event: Box::new(event),
            })
            .unwrap();
        assert!(app.poll_subagent_events());
        assert!(!app.sync_stream_presentation(Instant::now()));
        app.layout_workspace_frame(Size::new(960.0, 540.0));
        assert!(
            (app.tree.scroll_offset("transcript") - previous_offset).abs() < 0.1,
            "foreign layout owner: {previous_owner_is_foreign}, before: {previous_offset}, after: {}",
            app.tree.scroll_offset("transcript")
        );
        assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
        if !previous_owner_is_foreign {
            assert_eq!(app.view.transcript, previous_transcript);
        }
    }
}

#[tokio::test]
async fn selected_live_child_resize_preserves_following_but_not_manual_or_foreign_reading() {
    for follow in [true, false] {
        let mut fixture = ChildFixture::start_with_prompt(&scroll_history()).await;
        fixture.app.open_session(&fixture.child_id);
        fixture.drain_host();
        fixture.app.view.sidebar_collapsed = true;
        fixture.app.view.show_side_pane = false;
        fixture.app.reduced_motion = true;
        fixture.app.tree.set_reduced_motion(true);
        let frame = scroll_frame(&mut fixture.app, 620.0);
        assert!(fixture.app.tree.is_scroll_at_end("transcript", 0.1));
        if !follow {
            scroll_up(&mut fixture.app, &frame);
            scroll_frame(&mut fixture.app, 620.0);
        }
        let previous_offset = fixture.app.tree.scroll_offset("transcript");
        assert!(!fixture.app.view.running);
        assert!(!fixture.app.view.background_run_selected);
        assert!(!fixture.app.sync_stream_presentation(Instant::now()));
        fixture.app.layout_workspace_frame(Size::new(960.0, 520.0));
        if follow {
            assert!(fixture.app.tree.is_scroll_at_end("transcript", 0.1));
            assert!(fixture.app.tree.scroll_offset("transcript") > previous_offset + 50.0);
        } else {
            assert!((fixture.app.tree.scroll_offset("transcript") - previous_offset).abs() < 0.1);
            assert!(!fixture.app.tree.is_scroll_at_end("transcript", 24.0));
        }
        fixture.app.view.selected_session = Some("other-owner".into());
        fixture.app.transcript_layout_session = fixture.app.view.selected_session.clone();
        fixture.app.tree.scroll_to_end("transcript");
        scroll_frame(&mut fixture.app, 520.0);
        let foreign_offset = fixture.app.tree.scroll_offset("transcript");
        fixture.app.layout_workspace_frame(Size::new(960.0, 440.0));
        assert!((fixture.app.tree.scroll_offset("transcript") - foreign_offset).abs() < 0.1);
        assert!(!fixture.app.tree.is_scroll_at_end("transcript", 24.0));
        fixture.model.flood.notify_one();
        fixture.finish().await;
    }
}
