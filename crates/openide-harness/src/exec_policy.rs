//! A bounded argv-prefix rule set for native command launch tools.
//!
//! This is an original, intentionally smaller implementation than Codex's
//! Starlark execpolicy. It matches the exact `command` and `args` supplied by
//! the model, without reparsing arbitrary shell scripts. Rules can tighten
//! host permissions to Ask or Deny; Allow answers a host Ask for its exact
//! prefix and never opens what the host denies.

use openide_core::ToolCall;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_EXEC_RULES: usize = 128;
pub const MAX_EXEC_PATTERN_TOKENS: usize = 32;
pub const MAX_EXEC_TOKEN_BYTES: usize = 1024;
pub const MAX_EXEC_JUSTIFICATION_BYTES: usize = 4096;
const MAX_COMMAND_ARGV_TOKENS: usize = 256;
const MAX_COMMAND_ARGV_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExecPatternToken {
    Literal(String),
    AnyOf(Vec<String>),
}

impl ExecPatternToken {
    fn alternatives(&self) -> &[String] {
        match self {
            Self::Literal(value) => std::slice::from_ref(value),
            Self::AnyOf(values) => values,
        }
    }

    fn matches(&self, actual: &str, first: bool, restrictive: bool) -> bool {
        self.alternatives().iter().any(|expected| {
            expected == actual
                || first
                    && restrictive
                    && executable_basename(expected) == executable_basename(actual)
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRule {
    pattern: Vec<ExecPatternToken>,
    decision: ExecDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    justification: Option<String>,
}

impl ExecRule {
    pub fn new(
        pattern: Vec<ExecPatternToken>,
        decision: ExecDecision,
        justification: Option<String>,
    ) -> Result<Self, ExecPolicyError> {
        let rule = Self {
            pattern,
            decision,
            justification,
        };
        rule.validate()?;
        Ok(rule)
    }

    pub fn pattern(&self) -> &[ExecPatternToken] {
        &self.pattern
    }

    pub fn decision(&self) -> ExecDecision {
        self.decision
    }

    pub fn justification(&self) -> Option<&str> {
        self.justification.as_deref()
    }

    fn validate(&self) -> Result<(), ExecPolicyError> {
        if self.pattern.is_empty() || self.pattern.len() > MAX_EXEC_PATTERN_TOKENS {
            return Err(ExecPolicyError::InvalidPattern(
                "prefix must have 1 to 32 argv tokens",
            ));
        }
        for token in &self.pattern {
            let alternatives = token.alternatives();
            if alternatives.is_empty() || alternatives.len() > MAX_EXEC_PATTERN_TOKENS {
                return Err(ExecPolicyError::InvalidPattern(
                    "a token union must have 1 to 32 alternatives",
                ));
            }
            for (index, value) in alternatives.iter().enumerate() {
                if value.trim().is_empty()
                    || value.len() > MAX_EXEC_TOKEN_BYTES
                    || value.contains('\0')
                {
                    return Err(ExecPolicyError::InvalidPattern(
                        "prefix token must be nonblank, bounded, and NUL-free",
                    ));
                }
                if alternatives[..index].contains(value) {
                    return Err(ExecPolicyError::InvalidPattern(
                        "token union contains a duplicate alternative",
                    ));
                }
            }
        }
        if self.justification.as_ref().is_some_and(|value| {
            value.trim().is_empty()
                || value.len() > MAX_EXEC_JUSTIFICATION_BYTES
                || value.contains('\0')
        }) {
            return Err(ExecPolicyError::InvalidPattern(
                "justification must be nonblank, bounded, and NUL-free",
            ));
        }
        Ok(())
    }

    fn matches(&self, argv: &[String]) -> bool {
        argv.len() >= self.pattern.len()
            && self
                .pattern
                .iter()
                .zip(argv)
                .enumerate()
                .all(|(index, (pattern, actual))| {
                    pattern.matches(actual, index == 0, self.decision != ExecDecision::Allow)
                })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExecPolicy {
    rules: Vec<ExecRule>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecEvaluation {
    /// None means no rule matched. Shell wrappers remain Ask or Deny when
    /// any rule is configured because their script contents are opaque.
    pub decision: Option<ExecDecision>,
    pub matched_rule_indices: Vec<usize>,
    pub opaque_wrapper: bool,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ExecPolicyError {
    #[error("too many command rules (maximum {MAX_EXEC_RULES})")]
    TooManyRules,
    #[error("invalid command prefix rule: {0}")]
    InvalidPattern(&'static str),
    #[error("tool call is not run_command or start_command")]
    NotRunCommand,
    #[error("run_command has invalid command or args")]
    InvalidCommandArgv,
}

impl ExecPolicy {
    pub fn new(rules: Vec<ExecRule>) -> Result<Self, ExecPolicyError> {
        if rules.len() > MAX_EXEC_RULES {
            return Err(ExecPolicyError::TooManyRules);
        }
        for rule in &rules {
            rule.validate()?;
        }
        Ok(Self { rules })
    }

    pub fn rules(&self) -> &[ExecRule] {
        &self.rules
    }

    pub fn has_deny_rule(&self) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.decision == ExecDecision::Deny)
    }

    pub fn evaluate_call(&self, call: &ToolCall) -> Result<ExecEvaluation, ExecPolicyError> {
        if !matches!(call.name.as_str(), "run_command" | "start_command") {
            return Err(ExecPolicyError::NotRunCommand);
        }
        // Rules judge the argv that will run, not an unsplit line.
        let split = crate::split_command_arguments(&call.arguments);
        let arguments = split.as_ref().unwrap_or(&call.arguments);
        let command = arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or(ExecPolicyError::InvalidCommandArgv)?;
        let mut argv = vec![command.to_owned()];
        match arguments.get("args") {
            Some(serde_json::Value::Array(args)) => {
                if args.len() >= MAX_COMMAND_ARGV_TOKENS {
                    return Err(ExecPolicyError::InvalidCommandArgv);
                }
                for arg in args {
                    argv.push(
                        arg.as_str()
                            .ok_or(ExecPolicyError::InvalidCommandArgv)?
                            .to_owned(),
                    );
                }
            }
            None => {}
            Some(_) => return Err(ExecPolicyError::InvalidCommandArgv),
        }
        self.evaluate_argv(&argv)
    }

    pub fn evaluate_argv(&self, argv: &[String]) -> Result<ExecEvaluation, ExecPolicyError> {
        if argv.is_empty()
            || argv.len() > MAX_COMMAND_ARGV_TOKENS
            || argv[0].trim().is_empty()
            || argv.iter().any(|token| token.contains('\0'))
            || argv
                .iter()
                .fold(0usize, |size, token| size.saturating_add(token.len()))
                > MAX_COMMAND_ARGV_BYTES
        {
            return Err(ExecPolicyError::InvalidCommandArgv);
        }
        let mut decision = None;
        let mut matched_rule_indices = Vec::new();
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.matches(argv) {
                matched_rule_indices.push(index);
                decision = Some(decision.unwrap_or(ExecDecision::Allow).max(rule.decision));
            }
        }
        let opaque_wrapper = !self.rules.is_empty() && is_opaque_wrapper(&argv[0]);
        if opaque_wrapper {
            // We do not split scripts or expand shell syntax. Any deny rule
            // makes an opaque wrapper a deny, so it cannot hide that prefix.
            decision = Some(if self.has_deny_rule() {
                ExecDecision::Deny
            } else {
                decision
                    .unwrap_or(ExecDecision::Allow)
                    .max(ExecDecision::Ask)
            });
        }
        Ok(ExecEvaluation {
            decision,
            matched_rule_indices,
            opaque_wrapper,
        })
    }

    /// Stable identity for a validated rule set, including its authored order
    /// and justifications. The host permission policy binds this digest with
    /// its own defaults and per-tool modes in the native event journal.
    pub fn digest_hex(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"openide-exec-policy-v1\0");
        hasher.update((self.rules.len() as u64).to_be_bytes());
        for rule in &self.rules {
            hasher.update([match rule.decision {
                ExecDecision::Allow => 0,
                ExecDecision::Ask => 1,
                ExecDecision::Deny => 2,
            }]);
            hasher.update((rule.pattern.len() as u64).to_be_bytes());
            for token in &rule.pattern {
                hasher.update([matches!(token, ExecPatternToken::AnyOf(_)) as u8]);
                hasher.update((token.alternatives().len() as u64).to_be_bytes());
                for value in token.alternatives() {
                    hash_field(&mut hasher, value);
                }
            }
            match &rule.justification {
                Some(value) => {
                    hasher.update([1]);
                    hash_field(&mut hasher, value);
                }
                None => hasher.update([0]),
            }
        }
        format!("{:x}", hasher.finalize())
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn executable_basename(program: &str) -> String {
    let basename = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let lowercase = basename.to_ascii_lowercase();
    lowercase
        .strip_suffix(".exe")
        .unwrap_or(&lowercase)
        .to_owned()
}

/// The rule "always allow" would save for a command call: the program and
/// its first plain argument (`cargo test`, `npm run`). None for anything else,
/// and for a shell or interpreter, which runs whatever follows.
pub fn always_allow_prefix(tool_name: &str, arguments: &serde_json::Value) -> Option<Vec<String>> {
    if !matches!(tool_name, "run_command" | "start_command") {
        return None;
    }
    let arguments = crate::split_command_arguments(arguments).unwrap_or_else(|| arguments.clone());
    let command = arguments.get("command")?.as_str()?.trim();
    if command.is_empty() || command.contains(char::is_whitespace) || is_opaque_wrapper(command) {
        return None;
    }
    let mut prefix = vec![command.to_owned()];
    let first = arguments
        .get("args")
        .and_then(serde_json::Value::as_array)
        .and_then(|args| args.first())
        .and_then(serde_json::Value::as_str);
    if let Some(first) = first.filter(|first| {
        !first.starts_with('-') && !first.contains(['/', '\\', '.', ' ']) && first.len() <= 64
    }) {
        prefix.push(first.to_owned());
    }
    Some(prefix)
}

/// A shell or launcher that runs what its arguments say: no prefix of its
/// argv tells what will run.
pub fn is_opaque_wrapper(program: &str) -> bool {
    let lowercase = program.to_ascii_lowercase();
    matches!(
        executable_basename(program).as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "cmd"
            | "powershell"
            | "pwsh"
            | "wscript"
            | "cscript"
            | "env"
            | "sudo"
            | "doas"
            | "python"
            | "python2"
            | "python3"
            | "py"
            | "node"
            | "ruby"
            | "perl"
            | "php"
            | "lua"
            | "deno"
            | "bun"
    ) || [".bat", ".cmd", ".ps1", ".sh"]
        .iter()
        .any(|suffix| lowercase.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(pattern: Vec<ExecPatternToken>, decision: ExecDecision) -> ExecRule {
        ExecRule::new(pattern, decision, None).unwrap()
    }

    fn literal(value: &str) -> ExecPatternToken {
        ExecPatternToken::Literal(value.into())
    }

    fn call(command: &str, args: &[&str]) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "run_command".into(),
            arguments: json!({"command": command, "args": args}),
        }
    }

    #[test]
    fn exact_argv_prefix_and_union_use_most_restrictive_match() {
        let policy = ExecPolicy::new(vec![
            rule(vec![literal("git")], ExecDecision::Allow),
            rule(
                vec![
                    literal("git"),
                    ExecPatternToken::AnyOf(vec!["push".into(), "reset".into()]),
                ],
                ExecDecision::Ask,
            ),
            rule(
                vec![literal("git"), literal("reset"), literal("--hard")],
                ExecDecision::Deny,
            ),
        ])
        .unwrap();
        assert_eq!(
            policy
                .evaluate_call(&call("git", &["reset", "--hard", "HEAD"]))
                .unwrap()
                .decision,
            Some(ExecDecision::Deny)
        );
        assert_eq!(
            policy
                .evaluate_call(&call("git", &["push", "origin"]))
                .unwrap()
                .decision,
            Some(ExecDecision::Ask)
        );
        assert_eq!(
            policy
                .evaluate_call(&call("git", &["status"]))
                .unwrap()
                .decision,
            Some(ExecDecision::Allow)
        );
        assert_eq!(
            policy
                .evaluate_call(&call("other", &["git", "reset", "--hard"]))
                .unwrap()
                .decision,
            None
        );
    }

    #[test]
    fn restrictive_rule_covers_absolute_exe_and_case_but_allow_does_not() {
        let policy = ExecPolicy::new(vec![rule(
            vec![literal("git"), literal("push")],
            ExecDecision::Deny,
        )])
        .unwrap();
        assert_eq!(
            policy
                .evaluate_call(&call("C:\\Program Files\\Git\\GIT.EXE", &["push"]))
                .unwrap()
                .decision,
            Some(ExecDecision::Deny)
        );
        let allow = ExecPolicy::new(vec![rule(vec![literal("git")], ExecDecision::Allow)]).unwrap();
        assert_eq!(
            allow
                .evaluate_call(&call("C:\\Program Files\\Git\\git.exe", &["status"]))
                .unwrap()
                .decision,
            None
        );
    }

    #[test]
    fn shell_wrappers_cannot_hide_a_deny_or_gain_auto_allow() {
        let deny = ExecPolicy::new(vec![rule(vec![literal("rm")], ExecDecision::Deny)]).unwrap();
        let wrapped = deny
            .evaluate_call(&call(
                "C:\\Windows\\System32\\cmd.exe",
                &["/C", "echo ok & rm x"],
            ))
            .unwrap();
        assert_eq!(wrapped.decision, Some(ExecDecision::Deny));
        assert!(wrapped.opaque_wrapper);
        let allow = ExecPolicy::new(vec![rule(vec![literal("git")], ExecDecision::Allow)]).unwrap();
        assert_eq!(
            allow
                .evaluate_call(&call("bash", &["-lc", "git status && rm -rf /"]))
                .unwrap()
                .decision,
            Some(ExecDecision::Ask)
        );
        for interpreter in [
            "python", "python3", "py", "node", "ruby", "perl", "php", "lua", "deno", "bun",
        ] {
            let result = deny
                .evaluate_call(&call(interpreter, &["-c", "run_hidden_effect()"]))
                .unwrap();
            assert_eq!(result.decision, Some(ExecDecision::Deny), "{interpreter}");
            assert!(result.opaque_wrapper);
        }
    }

    #[test]
    fn malformed_rule_and_command_are_rejected_before_dispatch() {
        assert!(matches!(
            ExecRule::new(vec![], ExecDecision::Deny, None),
            Err(ExecPolicyError::InvalidPattern(_))
        ));
        assert!(matches!(
            ExecRule::new(
                vec![ExecPatternToken::AnyOf(vec!["git".into(), "git".into()])],
                ExecDecision::Ask,
                None
            ),
            Err(ExecPolicyError::InvalidPattern(_))
        ));
        let policy = ExecPolicy::default();
        let mut invalid = call("git", &["status"]);
        invalid.arguments = json!({"command":"git", "args":[42]});
        assert_eq!(
            policy.evaluate_call(&invalid),
            Err(ExecPolicyError::InvalidCommandArgv)
        );
        invalid.arguments = json!({"command":"", "args":[]});
        assert_eq!(
            policy.evaluate_call(&invalid),
            Err(ExecPolicyError::InvalidCommandArgv)
        );
    }

    #[test]
    fn digest_changes_with_rule_content_and_is_stable() {
        let first = ExecPolicy::new(vec![rule(vec![literal("git")], ExecDecision::Ask)]).unwrap();
        let second = ExecPolicy::new(vec![rule(vec![literal("git")], ExecDecision::Deny)]).unwrap();
        assert_eq!(first.digest_hex(), first.clone().digest_hex());
        assert_ne!(first.digest_hex(), second.digest_hex());
        assert_eq!(first.digest_hex().len(), 64);
    }
}
