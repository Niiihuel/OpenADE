use super::*;
use openide_core::{AgentMessage, PlanSnapshot, PlanStep, PlanStepStatus};

fn plan(status: PlanStepStatus) -> PlanSnapshot {
    PlanSnapshot {
        explanation: Some("Revisión **incremental** del proyecto.".into()),
        plan: vec![PlanStep {
            step: "Revisar archivos 🦀".into(),
            status,
        }],
    }
}

fn event(sequence: u64, kind: AgentEventKind) -> AgentEvent {
    AgentEvent::new("plan-session", sequence, kind)
}

#[test]
fn plan_history_keeps_each_event_in_order_with_identical_updates_and_replay() {
    let pending = plan(PlanStepStatus::Pending);
    let completed = plan(PlanStepStatus::Completed);
    let kinds = vec![
        AgentEventKind::TurnStarted {
            prompt: "Prepará y ejecutá el plan".into(),
        },
        AgentEventKind::PlanProposed {
            plan: pending.clone(),
        },
        AgentEventKind::AssistantDelta {
            content: "Empezando.".into(),
        },
        AgentEventKind::PlanUpdated {
            tool_call_id: "update-1".into(),
            plan: pending.clone(),
        },
        AgentEventKind::PlanUpdated {
            tool_call_id: "update-2".into(),
            plan: pending.clone(),
        },
        AgentEventKind::AssistantDelta {
            content: " Terminado.".into(),
        },
        AgentEventKind::PlanUpdated {
            tool_call_id: "update-3".into(),
            plan: completed.clone(),
        },
        AgentEventKind::TurnCompleted {
            response: "Empezando. Terminado.".into(),
            steps: 1,
        },
    ];
    let events: Vec<_> = kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| event(index as u64 + 1, kind))
        .collect();
    let mut live = TranscriptProjection::new();
    let mut published = Vec::new();
    for event in &events {
        live.apply_event(event);
        live.sync_into(&mut published);
        assert_eq!(published, live.items());
    }
    assert_eq!(
        live.items(),
        TranscriptProjection::from_events(&events).items()
    );
    assert_eq!(live.items().len(), 7);
    assert!(matches!(&live.items()[1], TranscriptItem::PlanSnapshot {
        id, sequence: Some(2), plan, is_proposal: true, tool_call_id: None
    } if id == "plan:plan-session:2" && plan == &pending));
    for (index, sequence, call) in [(3, 4, "update-1"), (4, 5, "update-2"), (6, 7, "update-3")] {
        assert!(
            matches!(&live.items()[index], TranscriptItem::PlanSnapshot {
            sequence: Some(actual), is_proposal: false, tool_call_id: Some(id), ..
        } if *actual == sequence && id == call)
        );
    }
    assert!(
        matches!(&live.items()[6], TranscriptItem::PlanSnapshot { plan, .. } if plan == &completed)
    );
    assert!(
        matches!(&live.items()[2], TranscriptItem::Assistant { content, streaming: false } if content == "Empezando.")
    );
    assert!(
        matches!(&live.items()[5], TranscriptItem::Assistant { content, streaming: false } if content == " Terminado.")
    );
}

#[test]
fn plan_event_identity_deduplicates_retry_without_coalescing_distinct_events_or_turns() {
    let kind = AgentEventKind::PlanProposed {
        plan: plan(PlanStepStatus::Pending),
    };
    let first = event(1, kind.clone());
    let mut projection = TranscriptProjection::new();
    assert!(projection.apply_event(&first));
    assert!(!projection.apply_event(&first));
    projection.apply_kind(&AgentEventKind::TurnStarted {
        prompt: "Segundo turno".into(),
    });
    assert!(!projection.apply_event(&first));
    assert!(projection.apply_event(&event(2, kind.clone())));
    assert!(projection.apply_event(&AgentEvent::new("other-session", 1, kind.clone())));
    assert!(projection.apply_kind(&kind));
    assert!(projection.apply_kind(&kind));
    let ids: Vec<_> = projection
        .items()
        .iter()
        .filter_map(|row| match row {
            TranscriptItem::PlanSnapshot { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 5);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 5);
}

#[test]
fn invalid_plans_do_not_enter_history_or_consume_a_durable_identity() {
    let mut projection = TranscriptProjection::new();
    let invalid = PlanSnapshot {
        explanation: None,
        plan: vec![],
    };
    let proposal = AgentEventKind::PlanProposed {
        plan: invalid.clone(),
    };
    let update = AgentEventKind::PlanUpdated {
        tool_call_id: "invalid".into(),
        plan: invalid,
    };
    for kind in [proposal, update] {
        assert!(!projection.apply_kind(&kind));
        assert!(!projection.apply_event(&event(1, kind)));
    }
    assert!(projection.items().is_empty());
    assert!(projection.apply_event(&event(
        1,
        AgentEventKind::PlanProposed {
            plan: plan(PlanStepStatus::Pending),
        }
    )));
    assert_eq!(projection.items().len(), 1);
}

#[test]
fn plans_do_not_end_model_ownership_or_duplicate_committed_markdown_and_citations() {
    let mut projection = TranscriptProjection::new();
    projection.apply_event(&event(
        1,
        AgentEventKind::TurnStarted {
            prompt: "Revisar".into(),
        },
    ));
    projection.apply_event(&event(
        2,
        AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "model".into(),
        },
    ));
    projection.apply_event(&event(
        3,
        AgentEventKind::AssistantDelta {
            content: "**Primero**\n".into(),
        },
    ));
    let model = projection.active_model;
    let rows = projection.active_assistant_rows.clone();
    projection.apply_event(&event(
        4,
        AgentEventKind::PlanUpdated {
            tool_call_id: "plan-call".into(),
            plan: plan(PlanStepStatus::InProgress),
        },
    ));
    assert_eq!(projection.active_model, model);
    assert_eq!(projection.active_assistant_rows, rows);
    assert!(!projection.active_assistant_committed);
    assert!(matches!(
        &projection.items()[rows[0]],
        TranscriptItem::Assistant {
            streaming: true,
            ..
        }
    ));
    projection.apply_event(&event(
        5,
        AgentEventKind::AssistantDelta {
            content: "\n```rust\nlet x = 1;\n```".into(),
        },
    ));
    let response = "**Primero**\n\n```rust\nlet x = 1;\n```";
    let mut assistant = AgentMessage::assistant(response, vec![]);
    assistant.provider_output_items = vec![serde_json::json!({
        "type":"message", "content":[{"type":"output_text", "text":response,
        "annotations":[{"type":"url_citation", "title":"Referencia", "url":"https://example.com/reference", "start_index":0, "end_index":7}]}]
    })];
    projection.apply_event(&event(
        6,
        AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant,
            response_id: None,
            usage: None,
        },
    ));
    projection.apply_event(&event(
        7,
        AgentEventKind::AssistantMessage {
            content: response.into(),
        },
    ));
    projection.apply_event(&event(
        8,
        AgentEventKind::PlanProposed {
            plan: plan(PlanStepStatus::Completed),
        },
    ));
    projection.apply_event(&event(
        9,
        AgentEventKind::TurnCompleted {
            response: response.into(),
            steps: 1,
        },
    ));
    let text: String = projection
        .items()
        .iter()
        .filter_map(|row| match row {
            TranscriptItem::Assistant {
                content,
                streaming: false,
            } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, response);
    assert_eq!(
        projection
            .items()
            .iter()
            .filter(|row| matches!(row, TranscriptItem::PlanSnapshot { .. }))
            .count(),
        2
    );
    assert_eq!(projection.items().iter().filter(|row| matches!(row, TranscriptItem::WebCitation { url, .. } if url == "https://example.com/reference")).count(), 1);
}

#[test]
fn authoritative_text_correction_keeps_interleaved_plan_snapshots() {
    let mut projection = TranscriptProjection::new();
    projection.apply_kind(&AgentEventKind::AssistantDelta {
        content: "Borrador".into(),
    });
    projection.apply_event(&event(
        2,
        AgentEventKind::PlanUpdated {
            tool_call_id: "p1".into(),
            plan: plan(PlanStepStatus::Pending),
        },
    ));
    projection.apply_kind(&AgentEventKind::AssistantDelta {
        content: " posterior".into(),
    });
    projection.apply_event(&event(
        4,
        AgentEventKind::PlanUpdated {
            tool_call_id: "p2".into(),
            plan: plan(PlanStepStatus::Completed),
        },
    ));
    projection.apply_kind(&AgentEventKind::AssistantMessage {
        content: "Respuesta corregida".into(),
    });
    assert_eq!(projection.items().len(), 3);
    assert!(
        matches!(&projection.items()[0], TranscriptItem::Assistant { content, streaming: false } if content == "Respuesta corregida")
    );
    assert!(matches!(
        &projection.items()[1],
        TranscriptItem::PlanSnapshot {
            sequence: Some(2),
            ..
        }
    ));
    assert!(matches!(
        &projection.items()[2],
        TranscriptItem::PlanSnapshot {
            sequence: Some(4),
            ..
        }
    ));
}
