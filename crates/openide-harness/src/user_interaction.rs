//! Built-in tools whose result is something the user decides: the turn waits
//! for the user through the same durable request as a permission prompt.
//!
//! The request is journaled as `PermissionRequested`, so replay restores it
//! after a restart; it never times out. The host records what the user
//! answered in the session's decision journal before resolving the request,
//! and that recorded answer becomes the tool result. These tools have no
//! effect of their own, need no permission and run no hooks.

use crate::{ToolCatalog, ToolDefinition, ToolOutput};
use openide_core::{PermissionResolution, ToolCall};
use serde_json::json;
use std::collections::HashSet;

pub const ASK_USER_TOOL: &str = "ask_user";

pub fn ask_user_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: ASK_USER_TOOL.into(),
        description: "Ask the user one to four multiple-choice questions and wait for the answer. Use it only for a genuine fork that the user must decide: a preference, a trade-off between valid approaches, or a requirement that is ambiguous and expensive to guess wrong. Do not use it for anything you can find out yourself by reading the workspace or running a check, or to ask permission to continue. Put the option you recommend first. The user can always write a different answer instead of picking an option, so do not add an \"other\" option. The result gives, per question, the selected option labels and any text the user wrote, or says that the user skipped it.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "One to four questions, asked together.",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": {
                                "type": "string",
                                "description": "A label of a few words naming the decision, unique within the call.",
                                "minLength": 1,
                                "maxLength": 40
                            },
                            "question": {
                                "type": "string",
                                "description": "The complete question.",
                                "minLength": 1,
                                "maxLength": 600
                            },
                            "options": {
                                "type": "array",
                                "description": "Two to four distinct choices, the recommended one first.",
                                "minItems": 2,
                                "maxItems": 4,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {"type": "string", "minLength": 1, "maxLength": 80},
                                        "description": {
                                            "type": "string",
                                            "description": "One line on what choosing it means.",
                                            "maxLength": 200
                                        }
                                    },
                                    "required": ["label"],
                                    "additionalProperties": false
                                }
                            },
                            "multi_select": {
                                "type": "boolean",
                                "description": "Whether the user may pick more than one option."
                            }
                        },
                        "required": ["header", "question", "options", "multi_select"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        }),
    }
}

/// Every set of these built-ins a turn may have been admitted with, newest
/// first. A frozen catalog is one of them appended to the executor's tools,
/// or none; recovery matches the saved digest instead of assuming the latest.
pub fn generations() -> Vec<Vec<ToolDefinition>> {
    vec![vec![ask_user_tool_definition()]]
}

pub(crate) fn current() -> Vec<ToolDefinition> {
    generations().swap_remove(0)
}

pub fn is_user_interaction_tool(name: &str) -> bool {
    name == ASK_USER_TOOL
}

/// `catalog` followed by each generation, newest first.
pub fn catalogs_with_user_interaction(catalog: &ToolCatalog) -> Vec<ToolCatalog> {
    generations()
        .into_iter()
        .filter_map(|generation| {
            ToolCatalog::new(catalog.definitions().iter().cloned().chain(generation)).ok()
        })
        .collect()
}

/// Whether `call` is one of these built-ins exactly as this build defines it,
/// not an external tool that happens to share the name in an older catalog.
pub(crate) fn is_builtin(catalog: &ToolCatalog, call: &ToolCall) -> bool {
    is_user_interaction_tool(&call.name)
        && generations()
            .into_iter()
            .flatten()
            .any(|definition| catalog.get(&call.name) == Some(&definition))
}

/// What the schema cannot say. A rejected call is answered without asking.
pub(crate) fn invalid(call: &ToolCall) -> Option<ToolOutput> {
    let questions = call.arguments.get("questions")?.as_array()?;
    let mut headers = HashSet::new();
    for question in questions {
        let header = question.get("header")?.as_str()?.trim();
        if header.is_empty() || !headers.insert(header) {
            return Some(ToolOutput::error(
                "invalid ask_user arguments: every question needs its own non-empty header",
            ));
        }
        let mut labels = HashSet::new();
        for option in question.get("options")?.as_array()? {
            let label = option.get("label")?.as_str()?.trim();
            if label.is_empty() || !labels.insert(label) {
                return Some(ToolOutput::error(
                    "invalid ask_user arguments: option labels must be distinct and non-empty",
                ));
            }
        }
    }
    None
}

/// The tool result for a resolved request. `answer` is what the host
/// recorded for this request, verbatim: user-authored content.
pub(crate) fn outcome(
    call: &ToolCall,
    resolution: PermissionResolution,
    answer: Option<String>,
) -> ToolOutput {
    match (resolution, answer) {
        (PermissionResolution::Allowed, Some(answer)) => ToolOutput::success(answer),
        (PermissionResolution::Allowed, None) => ToolOutput::error(format!(
            "The request was approved without an answer for {}. Proceed with your best judgement and say what you assumed.",
            call.name
        )),
        (PermissionResolution::Denied, _) => ToolOutput::success(
            "The user skipped this question without answering. Proceed with your best judgement and say what you assumed.",
        ),
        (PermissionResolution::TimedOut, _) => {
            ToolOutput::error(format!("Request timed out for {}", call.name))
        }
        (PermissionResolution::Cancelled, _) => {
            ToolOutput::error(format!("Request cancelled for {}", call.name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: ASK_USER_TOOL.into(),
            arguments,
        }
    }

    fn question(header: &str, labels: &[&str]) -> serde_json::Value {
        json!({
            "header": header,
            "question": "Which one?",
            "options": labels.iter().map(|label| json!({"label": label})).collect::<Vec<_>>(),
            "multi_select": false
        })
    }

    #[test]
    fn schema_is_strict_and_bounds_questions_and_options() {
        let catalog = ToolCatalog::new(current()).unwrap();
        let valid = call(json!({"questions": [question("Storage", &["SQLite", "Files"])]}));
        assert!(catalog.require_valid_call(&valid).is_ok());
        assert!(invalid(&valid).is_none());
        assert!(is_builtin(&catalog, &valid));
        for arguments in [
            json!({"questions": []}),
            json!({"questions": [question("A", &["only"])]}),
            json!({"questions": [question("A", &["1", "2", "3", "4", "5"])]}),
            json!({"questions": (0..5).map(|i| question(&i.to_string(), &["a", "b"])).collect::<Vec<_>>()}),
            json!({"questions": [question("A", &["a", "b"])], "extra": true}),
            json!({"questions": [{"header": "A", "question": "?", "options": [{"label": "a"}, {"label": "b"}]}]}),
        ] {
            assert!(catalog.require_valid_call(&call(arguments.clone())).is_err(), "{arguments}");
        }
        assert!(invalid(&call(json!({"questions": [question("A", &["same", "same"])]}))).is_some());
        assert!(invalid(&call(json!({
            "questions": [question("A", &["a", "b"]), question("A", &["a", "b"])]
        })))
        .is_some());
    }

    #[test]
    fn the_recorded_answer_is_the_result_and_a_skip_is_not_an_error() {
        let call = call(json!({"questions": [question("A", &["a", "b"])]}));
        let answered = outcome(&call, PermissionResolution::Allowed, Some("{\"answers\":[]}".into()));
        assert_eq!((answered.content.as_str(), answered.is_error), ("{\"answers\":[]}", false));
        let skipped = outcome(&call, PermissionResolution::Denied, None);
        assert!(!skipped.is_error && skipped.content.contains("skipped"));
        assert!(outcome(&call, PermissionResolution::Allowed, None).is_error);
    }
}
