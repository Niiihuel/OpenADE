//! Named roles a parent can give a subagent.
//!
//! A role only narrows a child: it adds a preamble to the child's prompt and
//! may make it read-only. It never grants tools, a model or permissions the
//! host did not already give subagents. The table is compiled in so the same
//! role resolves to the same prompt and mode when an interrupted child is
//! recovered from the journal.

use openide_core::TurnMode;

pub(crate) struct SubagentRole {
    pub name: &'static str,
    pub description: &'static str,
    preamble: &'static str,
    read_only: bool,
}

pub(crate) const ROLES: [SubagentRole; 3] = [
    SubagentRole {
        name: "explorer",
        description: "Read-only investigation of the codebase. Cannot edit files or run commands.",
        preamble: "You are an explorer subagent. Investigate and report; you cannot change anything. Answer with the specific files, symbols and line numbers that support each finding, and say plainly what you did not check.",
        read_only: true,
    },
    SubagentRole {
        name: "reviewer",
        description: "Read-only review of a change, with fresh context.",
        preamble: "You are a reviewer subagent with no stake in the change. Look for defects, not style: wrong behaviour, missed callers, missing tests, security and data-loss risks. Report each finding with its file and line, how it fails, and how sure you are. Report nothing you could not point to in the code.",
        read_only: true,
    },
    SubagentRole {
        name: "worker",
        description: "Carries out one well-scoped change, then verifies it.",
        preamble: "You are a worker subagent. Make exactly the change described, the smallest one that fully works, and touch nothing else. Before reporting, run the relevant check and state its result; if you could not verify something, say so.",
        read_only: false,
    },
];

/// The prompt and mode a spawn request runs with. No role is the child the
/// host always offered. An unknown role is an error for the model to correct.
pub(crate) fn resolve(prompt: &str, role: Option<&str>) -> Result<(String, TurnMode), String> {
    let Some(name) = role else {
        return Ok((prompt.to_owned(), TurnMode::Build));
    };
    let role = ROLES.iter().find(|role| role.name == name).ok_or_else(|| {
        format!(
            "unknown subagent role `{name}`; available roles: {}",
            ROLES.iter().map(|role| role.name).collect::<Vec<_>>().join(", ")
        )
    })?;
    Ok((
        format!("{}\n\nTask:\n{prompt}", role.preamble),
        if role.read_only {
            TurnMode::Plan
        } else {
            TurnMode::Build
        },
    ))
}

/// Text for the tool description, so the model can pick a role.
pub(crate) fn catalog() -> String {
    ROLES
        .iter()
        .map(|role| format!("`{}`: {}", role.name, role.description))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_role_is_the_unchanged_build_child() {
        assert_eq!(resolve("do it", None), Ok(("do it".to_owned(), TurnMode::Build)));
    }

    #[test]
    fn read_only_roles_run_in_plan_mode_and_keep_the_task_verbatim() {
        for role in &ROLES {
            let (prompt, mode) = resolve("inspect src/lib.rs", Some(role.name)).unwrap();
            assert!(prompt.ends_with("Task:\ninspect src/lib.rs"), "{prompt}");
            assert!(prompt.starts_with(role.preamble));
            assert_eq!(mode == TurnMode::Plan, role.read_only, "{}", role.name);
        }
        assert!(ROLES.iter().any(|role| role.read_only) && ROLES.iter().any(|role| !role.read_only));
    }

    #[test]
    fn unknown_roles_are_refused_and_name_the_alternatives() {
        let error = resolve("x", Some("admin")).unwrap_err();
        assert!(error.contains("admin") && error.contains("explorer") && error.contains("worker"));
        assert!(resolve("x", Some("")).is_err());
    }

    #[test]
    fn resolution_is_deterministic_so_recovery_rebuilds_the_same_child() {
        assert_eq!(resolve("t", Some("reviewer")), resolve("t", Some("reviewer")));
        let names: std::collections::HashSet<_> = ROLES.iter().map(|role| role.name).collect();
        assert_eq!(names.len(), ROLES.len());
    }
}
