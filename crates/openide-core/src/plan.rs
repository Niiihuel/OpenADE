//! Bounded, structured task plan stored independently of model history.
//!
//! The shape follows Codex's update_plan arguments (protocol/src/plan_tool.rs):
//! a full list of steps and statuses, with an optional explanation. Each event
//! is a complete snapshot, so compaction never has to infer plan state.

use serde::{Deserialize, Serialize};

pub const MAX_PLAN_STEPS: usize = 24;
pub const MAX_PLAN_STEP_BYTES: usize = 512;
pub const MAX_PLAN_EXPLANATION_BYTES: usize = 16 * 1024;
pub const MAX_PLAN_SNAPSHOT_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStep {
    pub step: String,
    pub status: PlanStepStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub plan: Vec<PlanStep>,
}

impl PlanSnapshot {
    pub fn is_valid(&self) -> bool {
        !self.plan.is_empty()
            && self.plan.len() <= MAX_PLAN_STEPS
            && self
                .plan
                .iter()
                .all(|item| !item.step.trim().is_empty() && item.step.len() <= MAX_PLAN_STEP_BYTES)
            && self
                .plan
                .iter()
                .filter(|item| item.status == PlanStepStatus::InProgress)
                .count()
                <= 1
            && self
                .explanation
                .as_ref()
                .is_none_or(|text| text.len() <= MAX_PLAN_EXPLANATION_BYTES)
            && serde_json::to_vec(self)
                .is_ok_and(|encoded| encoded.len() <= MAX_PLAN_SNAPSHOT_BYTES)
    }

    /// Preserve Plan mode's final text as a bounded explanation and extract
    /// its ordered or bulleted steps. A prose answer remains one pending step.
    pub fn from_plan_response(content: &str) -> Self {
        let bounded = truncate_utf8(content.trim(), MAX_PLAN_EXPLANATION_BYTES);
        let mut steps = bounded
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let item = line
                    .strip_prefix("- ")
                    .or_else(|| line.strip_prefix("* "))
                    .or_else(|| {
                        let (prefix, item) = line.split_once(". ")?;
                        (!prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_digit()))
                            .then_some(item)
                    })?;
                let item = truncate_utf8(item.trim(), MAX_PLAN_STEP_BYTES);
                (!item.is_empty()).then_some(PlanStep {
                    step: item,
                    status: PlanStepStatus::Pending,
                })
            })
            .take(MAX_PLAN_STEPS)
            .collect::<Vec<_>>();
        if steps.is_empty() {
            steps.push(PlanStep {
                step: truncate_utf8(
                    bounded
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("Review plan"),
                    MAX_PLAN_STEP_BYTES,
                ),
                status: PlanStepStatus::Pending,
            });
        }
        Self {
            explanation: Some(bounded),
            plan: steps,
        }
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_snapshot_is_bounded_and_has_at_most_one_active_step() {
        let mut plan = PlanSnapshot::from_plan_response("1. Inspect files\n2. Make change");
        assert!(plan.is_valid());
        assert_eq!(plan.plan.len(), 2);
        plan.plan[0].status = PlanStepStatus::InProgress;
        plan.plan[1].status = PlanStepStatus::InProgress;
        assert!(!plan.is_valid());
        plan.plan[1].status = PlanStepStatus::Pending;
        plan.plan[0].step = "x".repeat(MAX_PLAN_STEP_BYTES + 1);
        assert!(!plan.is_valid());
        let long_response = format!("1. {}\n{}", "x".repeat(4_000), "y".repeat(50_000));
        let proposed = PlanSnapshot::from_plan_response(&long_response);
        assert!(proposed.is_valid());
        assert!(proposed.explanation.as_ref().unwrap().len() <= MAX_PLAN_EXPLANATION_BYTES);
        assert!(proposed.plan[0].step.len() <= MAX_PLAN_STEP_BYTES);
    }
}
