//! Journal-backed built-in plan progress tool.
//!
//! Matches the Codex update_plan argument shape in
//! references/codex/codex-rs/protocol/src/plan_tool.rs and its Build-only
//! rejection in core/src/tools/handlers/plan.rs.

use crate::{ToolDefinition, ToolOutput};
use openide_core::{PlanSnapshot, ToolCall};
use serde_json::json;

pub(crate) const UPDATE_PLAN_TOOL: &str = "update_plan";

pub fn update_plan_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: UPDATE_PLAN_TOOL.into(),
        description: "Update the current task plan. Provide the full list of steps and statuses; at most one step may be in_progress.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "explanation": {"type": "string", "description": "Optional explanation for the update."},
                "plan": {
                    "type": "array",
                    "description": "The complete ordered list of steps.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                        },
                        "required": ["step", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["plan"],
            "additionalProperties": false
        }),
    }
}

pub(crate) fn definition() -> ToolDefinition {
    update_plan_tool_definition()
}

pub(crate) fn parse(call: &ToolCall) -> Result<PlanSnapshot, ToolOutput> {
    let plan = serde_json::from_value::<PlanSnapshot>(call.arguments.clone())
        .map_err(|error| ToolOutput::error(format!("invalid update_plan arguments: {error}")))?;
    if !plan.is_valid() {
        return Err(ToolOutput::error(
            "invalid update_plan arguments: plan exceeds bounds, contains an empty step, or has multiple in_progress steps",
        ));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{PlanStep, PlanStepStatus};

    #[test]
    fn plan_tool_arguments_match_codex_shape() {
        let plan = PlanSnapshot {
            explanation: None,
            plan: vec![PlanStep {
                step: "Inspect files".into(),
                status: PlanStepStatus::InProgress,
            }],
        };
        let call = ToolCall {
            id: "call-1".into(),
            name: UPDATE_PLAN_TOOL.into(),
            arguments: serde_json::to_value(&plan).unwrap(),
        };
        assert_eq!(parse(&call).unwrap(), plan);
        assert!(parse(&ToolCall {
            arguments: json!({"plan": [{"step": "a", "status": "in_progress"}, {"step": "b", "status": "in_progress"}]}),
            ..call
        }).is_err());
    }
}
