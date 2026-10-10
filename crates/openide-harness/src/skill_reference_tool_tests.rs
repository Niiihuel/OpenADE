use super::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn opened_reference_location_rejects_parent_replacement() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let skill = root.join("skill");
    let rules = skill.join("rules");
    let outside = root.join("outside");
    fs::create_dir_all(&rules).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(rules.join("rule.md"), "safe").unwrap();
    fs::write(outside.join("rule.md"), "private").unwrap();
    let checked = existing_workspace_path(&skill, "rules/rule.md").unwrap();
    fs::rename(&rules, skill.join("previous")).unwrap();
    symlink(&outside, &rules).unwrap();
    let opened = open_no_follow(&checked).unwrap();
    assert!(verify_opened_file_in_workspace(&opened, &skill).is_err());
    let safe = open_no_follow(&skill.join("previous/rule.md")).unwrap();
    assert!(verify_opened_file_in_workspace(&safe, &skill).is_ok());
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "skill-reference".into(),
        name: name.into(),
        arguments,
    }
}

#[tokio::test]
async fn installed_skill_references_are_read_only_and_explicitly_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("project");
    let skill = temp.path().join("installed/active-skill");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("AGENTS.md"), "Reference rules").unwrap();
    fs::write(temp.path().join("private.txt"), "private").unwrap();
    let skill = skill.canonicalize().unwrap();
    let context = ToolContext {
        workspace,
        cancel: CancellationToken::new(),
    };
    let tools = LocalTools::new()
        .with_write_access(true)
        .with_read_only_roots(vec![skill.clone()]);
    let reference = skill.join("AGENTS.md");
    let read = call("read_file", json!({"path": reference}));
    fs::write(
        skill.join("../AGENTS.md"),
        "External instructions must not activate",
    )
    .unwrap();
    assert!(
        crate::project_instructions::ProjectInstructionLedger::default()
            .preflight(&context.workspace, &read, 16_000)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        tools.execute(&read, &context).await.unwrap(),
        ToolOutput::success("Reference rules")
    );
    // No root is inferred from a model-supplied absolute path or a prompt.
    assert!(LocalTools::new().execute(&read, &context).await.is_err());
    for request in [
        call(
            "read_file",
            json!({"path": temp.path().join("private.txt")}),
        ),
        call(
            "read_file",
            json!({"path": skill.join("../active-skill/AGENTS.md")}),
        ),
        call("list_files", json!({"path": skill})),
        call("search_files", json!({"path": skill, "pattern": "rules"})),
        call(
            "write_file",
            json!({"path": reference, "content": "modified"}),
        ),
    ] {
        assert!(
            tools.execute(&request, &context).await.is_err(),
            "{}",
            request.name
        );
    }
    assert_eq!(fs::read_to_string(&reference).unwrap(), "Reference rules");
    let disabled = tools.with_tool_permission(LocalTool::ReadFile, false);
    assert!(disabled.execute(&read, &context).await.unwrap().is_error);
    assert!(!disabled
        .definitions()
        .iter()
        .any(|tool| tool.name == "read_file"));
}

#[cfg(unix)]
#[tokio::test]
async fn installed_reference_reads_reject_links_and_replaced_roots() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("project");
    let skill = temp.path().join("skill");
    let outside = temp.path().join("outside");
    for path in [&workspace, &skill, &outside] {
        fs::create_dir(path).unwrap();
    }
    fs::write(outside.join("secret.md"), "private").unwrap();
    symlink(outside.join("secret.md"), skill.join("reference.md")).unwrap();
    let skill = skill.canonicalize().unwrap();
    let tools = LocalTools::new().with_read_only_roots(vec![skill.clone()]);
    let context = ToolContext {
        workspace,
        cancel: CancellationToken::new(),
    };
    assert!(tools
        .execute(
            &call("read_file", json!({"path": skill.join("reference.md")})),
            &context
        )
        .await
        .is_err());
    fs::remove_file(skill.join("reference.md")).unwrap();
    fs::remove_dir(&skill).unwrap();
    symlink(&outside, &skill).unwrap();
    assert!(tools
        .execute(
            &call("read_file", json!({"path": skill.join("secret.md")})),
            &context
        )
        .await
        .is_err());
}

#[tokio::test]
async fn progressive_catalog_can_read_skills_beyond_the_old_thirty_two_root_limit() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("project");
    fs::create_dir(&workspace).unwrap();
    let roots: Vec<_> = (0..40)
        .map(|index| {
            let root = temp.path().join(format!("skill-{index:02}"));
            fs::create_dir(&root).unwrap();
            fs::write(root.join("SKILL.md"), format!("Full instructions {index}")).unwrap();
            root.canonicalize().unwrap()
        })
        .collect();
    let tools = LocalTools::new()
        .with_write_access(true)
        .with_read_only_roots(roots.clone());
    let context = ToolContext {
        workspace,
        cancel: CancellationToken::new(),
    };
    let read = call("read_file", json!({"path": roots[39].join("SKILL.md")}));
    assert_eq!(
        tools.execute(&read, &context).await.unwrap(),
        ToolOutput::success("Full instructions 39")
    );
    assert!(tools
        .execute(
            &call(
                "write_file",
                json!({"path": roots[39].join("SKILL.md"), "content":"changed"})
            ),
            &context
        )
        .await
        .is_err());
    let disabled = tools.with_tool_permission(LocalTool::ReadFile, false);
    assert!(disabled.execute(&read, &context).await.unwrap().is_error);
}
