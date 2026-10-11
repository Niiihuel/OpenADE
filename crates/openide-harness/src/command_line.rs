//! One reading of `{command, args}` for approval, policy, hooks and spawn.
//!
//! Commands never run through a shell. A model that writes a whole line in
//! `command` gets it split only when no shell could have changed its meaning.

use crate::{AgentError, ToolContext, ToolDefinition, ToolExecutor, ToolOutput};
use async_trait::async_trait;
use openide_core::ToolCall;
use serde_json::Value;
use std::{path::Path, sync::Arc};

/// Appended to the description of every tool that takes `command` + `args`.
pub(crate) const COMMAND_RULE: &str = " `command` is the program name alone; every argument goes in `args`. No shell: quotes, pipes, redirections and variables are not interpreted.";

const NEEDS_SHELL: &str = "`command` must be the executable alone; pass arguments in `args` (command: \"npm\", args: [\"run\", \"dev\"]). Shell syntax is not interpreted.";

enum Line {
    AsGiven,
    Words(Vec<String>),
    NeedsShell,
}

fn classify(arguments: &Value) -> Line {
    let Some(command) = arguments.get("command").and_then(Value::as_str) else {
        return Line::AsGiven;
    };
    let has_args = match arguments.get("args") {
        None => false,
        Some(Value::Array(args)) => !args.is_empty(),
        Some(_) => true,
    };
    let mut words = command.split_whitespace();
    let (Some(program), Some(_)) = (words.next(), words.next()) else {
        return Line::AsGiven;
    };
    // A first word with a separator may be a real path that contains spaces;
    // explicit `args` mean the caller already separated program and arguments.
    if has_args || program.contains(['/', '\\']) {
        return Line::AsGiven;
    }
    if command.contains(|ch: char| "\"'|&;<>()$`\\*?~#!{}[]\n\r".contains(ch))
        || program.contains('=')
        || command.split_whitespace().any(|word| word.starts_with('='))
    {
        return Line::NeedsShell;
    }
    Line::Words(command.split_whitespace().map(str::to_owned).collect())
}

/// The arguments that will actually run when `command` held plain words and
/// `args` was absent or empty; `None` when the call runs exactly as written.
/// Approval keys, command rules and hooks read this same value.
pub fn split_command_arguments(arguments: &Value) -> Option<Value> {
    let Line::Words(words) = classify(arguments) else {
        return None;
    };
    let mut arguments = arguments.clone();
    let object = arguments.as_object_mut()?;
    object.insert("command".into(), words[0].clone().into());
    object.insert("args".into(), words[1..].to_vec().into());
    Some(arguments)
}

/// The call to execute, or the precise refusal for a line that needs a shell.
pub(crate) fn executed_call(call: &ToolCall) -> Result<Option<ToolCall>, AgentError> {
    if matches!(classify(&call.arguments), Line::NeedsShell) {
        return Err(AgentError::Tool(NEEDS_SHELL.into()));
    }
    Ok(split_command_arguments(&call.arguments).map(|arguments| ToolCall {
        arguments,
        ..call.clone()
    }))
}

/// What a split call ran, for the tool result the model and transcript see.
pub(crate) fn executed_words(call: &ToolCall) -> Vec<String> {
    call.arguments["command"]
        .as_str()
        .into_iter()
        .chain(
            call.arguments["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
        .map(str::to_owned)
        .collect()
}

pub(crate) fn missing_working_directory(cwd: &Path) -> Option<AgentError> {
    (!cwd.is_dir()).then(|| {
        AgentError::Tool(format!(
            "working directory does not exist: {}",
            cwd.display()
        ))
    })
}

/// Names what was missing; never echoes environment values.
pub(crate) fn spawn_error(context: &str, program: &str, error: &std::io::Error) -> AgentError {
    if error.kind() != std::io::ErrorKind::NotFound {
        return AgentError::Tool(format!("{context}: {error}"));
    }
    let program: String = program.chars().take(256).collect();
    let mut message = if program.contains(['/', '\\']) {
        format!("{context}: executable not found: {program}")
    } else {
        format!("{context}: executable not found on PATH: {program}")
    };
    if program.contains(char::is_whitespace) {
        message.push_str(". ");
        message.push_str(NEEDS_SHELL);
    }
    AgentError::Tool(message)
}

/// A current command tool definition as the catalog before the command rule
/// and `timeout_ms` advertised it.
pub fn definition_before_command_revision(mut definition: ToolDefinition) -> ToolDefinition {
    definition.description = definition.description.replace(COMMAND_RULE, "");
    if crate::tools::persistent_command_tools::is_persistent_tool(&definition.name) {
        if let Some(properties) = definition.parameters["properties"].as_object_mut() {
            properties.remove("timeout_ms");
        }
    }
    definition
}

/// Advertises the previous catalog to a child reserved under it. Execution is
/// unchanged: nothing the old catalog offered was removed.
pub(crate) struct BeforeCommandRevision(pub(crate) Arc<dyn ToolExecutor>);

#[async_trait]
impl ToolExecutor for BeforeCommandRevision {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.0
            .definitions()
            .into_iter()
            .map(definition_before_command_revision)
            .collect()
    }
    fn certifies_parallel_read(&self, name: &str) -> bool {
        self.0.certifies_parallel_read(name)
    }
    fn certifies_plan_read(&self, name: &str) -> bool {
        self.0.certifies_plan_read(name)
    }
    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        self.0.execute(call, context).await
    }
    async fn execute_streaming(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        output: crate::ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        self.0.execute_streaming(call, context, output).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_plain_words_are_split_and_shell_syntax_is_refused_with_the_hint() {
        assert_eq!(
            split_command_arguments(&json!({"command":"npm  run dev","timeout_ms":1000})),
            Some(json!({"command":"npm","args":["run","dev"],"timeout_ms":1000}))
        );
        assert_eq!(
            split_command_arguments(&json!({"command":"npm run dev","args":[]})),
            Some(json!({"command":"npm","args":["run","dev"]}))
        );
        for unchanged in [
            json!({"command":"npm"}),
            json!({"command":"npm run","args":["dev"]}),
            json!({"command":"/opt/My Tools/run now"}),
            json!({"command":"C:\\Program Files\\Git\\git.exe"}),
            json!({"args":["x"]}),
        ] {
            assert_eq!(split_command_arguments(&unchanged), None, "{unchanged}");
            let call = ToolCall {
                id: "c".into(),
                name: "run_command".into(),
                arguments: unchanged,
            };
            assert!(executed_call(&call).unwrap().is_none());
        }
        for line in [
            "echo 'a b'",
            "echo \"a\"",
            "ls | wc",
            "a && b",
            "a; b",
            "cat < f",
            "echo > f",
            "(a b)",
            "echo $HOME",
            "echo `id`",
            "echo a\\ b",
            "ls *.rs",
            "ls a?",
            "ls ~",
            "echo # x",
            "echo !x",
            "echo {a,b}",
            "ls [ab]",
            "FOO=1 npm start",
            "npm =x",
            "echo a\nrm b",
        ] {
            let call = ToolCall {
                id: "c".into(),
                name: "start_command".into(),
                arguments: json!({"command":line}),
            };
            assert_eq!(split_command_arguments(&call.arguments), None, "{line}");
            let error = executed_call(&call).unwrap_err().to_string();
            assert!(
                error.contains("pass arguments in `args`") && error.contains("Shell syntax"),
                "{line}: {error}"
            );
        }
        // An assignment-looking argument after the program is an argument.
        assert!(split_command_arguments(&json!({"command":"env A=1"})).is_some());
    }

    #[test]
    fn missing_executable_and_missing_directory_are_told_apart() {
        let not_found = std::io::Error::from(std::io::ErrorKind::NotFound);
        let error = spawn_error("cannot start command", "nope-tool", &not_found).to_string();
        assert!(error.contains("executable not found on PATH: nope-tool"), "{error}");
        let error = spawn_error("cannot start command", "./bin/nope", &not_found).to_string();
        assert!(error.contains("executable not found: ./bin/nope"), "{error}");
        let error = spawn_error("cannot start command", "./my tool", &not_found).to_string();
        assert!(error.contains("pass arguments in `args`"), "{error}");
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(!spawn_error("x", "tool", &denied).to_string().contains("not found"));
        let root = tempfile::tempdir().unwrap();
        assert!(missing_working_directory(root.path()).is_none());
        let gone = root.path().join("gone");
        let error = missing_working_directory(&gone).unwrap().to_string();
        assert!(error.contains("working directory does not exist") && error.contains("gone"));
    }
}
