//! Result navigation is resolved against the active transcript at click time.
use super::*;
use crate::search_activity_ui;

impl AgentsWindow {
    pub(super) fn apply_activity_navigation(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if let Some(raw) = id.strip_prefix("activity_page:") {
            let Ok((owner, row)) = serde_json::from_str::<(String, String)>(raw) else {
                return true;
            };
            if self.view.selected_session.as_deref() != Some(&owner) {
                return true;
            }
            let url = self.view.transcript.iter().find_map(|item| match item {
                TranscriptItem::WebSearch {
                    id,
                    action,
                    url: Some(url),
                    ..
                } if id == &row
                    && search_activity_ui::is_page_action(action)
                    && search_activity_ui::valid_web_url(url) =>
                {
                    Some(url.clone())
                }
                _ => None,
            });
            if let Some(url) = url {
                ctx.open_url(egui::OpenUrl::new_tab(url));
            }
            return true;
        }
        let Some(raw) = id.strip_prefix("activity_result:") else {
            return false;
        };
        let Ok((owner, row, index, hosted)) =
            serde_json::from_str::<(String, String, usize, bool)>(raw)
        else {
            return true;
        };
        if self.view.selected_session.as_deref() != Some(&owner) {
            return true;
        }
        if !hosted && !self.tool_navigation_is_unique(&owner, &row) {
            return true;
        }
        let target = self.view.transcript.iter().find_map(|item| match item {
            TranscriptItem::WebSearch { id, sources, .. } if hosted && id == &row => {
                sources.get(index).map(|(_, url)| (url.clone(), true))
            }
            TranscriptItem::Tool {
                id,
                name,
                arguments,
                output: Some(output),
                is_error: false,
                activity: crate::transcript::ActivityStatus::Completed,
            } if !hosted && id == &row && search_activity_ui::supported(name) => {
                search_activity_ui::captured_results(name, output, arguments)
                    .get(index)
                    .filter(|result| !result.directory)
                    .map(|result| (result.target.clone(), result.web))
            }
            _ => None,
        });
        let Some((target, web)) = target else {
            return true;
        };
        if web {
            if search_activity_ui::valid_web_url(&target) {
                ctx.open_url(egui::OpenUrl::new_tab(target));
            }
        } else {
            let workspace = Path::new(&self.view.workspace);
            let relative = workspace.canonicalize().ok().and_then(|root| {
                let path = workspace.join(&target).canonicalize().ok()?;
                path.is_file().then_some(())?;
                path.strip_prefix(root).ok().map(Path::to_path_buf)
            });
            if let Some(relative) = relative {
                self.close_environment();
                if self
                    .dock
                    .open_file(Path::new(&self.view.workspace), &owner, &relative)
                    .is_some()
                {
                    self.sync_dock_view();
                }
            } else {
                self.view.status = "El archivo no está disponible dentro del proyecto".into();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::ModelStreamStatus;
    #[test]
    fn hosted_sources_are_resolved_from_the_current_row_and_only_open_http_urls() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.selected_session = Some("owner".into());
        app.view.transcript.push(TranscriptItem::WebSearch {
            id: "web:1".into(),
            item_id: "web".into(),
            action: "search".into(),
            query: Some("Rust".into()),
            url: None,
            sources: vec![
                ("Docs".into(), "https://example.com/docs".into()),
                ("Bad".into(), "file:///etc/passwd".into()),
            ],
            status: ModelStreamStatus::Committed,
        });
        for (owner, row, index, expected) in [
            ("owner", "web:1", 0, true),
            ("stale", "web:1", 0, false),
            ("owner", "web:missing", 0, false),
            ("owner", "web:1", 1, false),
            ("owner", "web:1", 99, false),
        ] {
            let ctx = egui::Context::default();
            ctx.begin_pass(egui::RawInput::default());
            assert!(app.apply_activity_navigation(
                &UiAction::Clicked {
                    id: search_activity_ui::result_action(owner, row, index, true),
                },
                &ctx
            ));
            let mut output = ctx.end_pass();
            assert_eq!(
                output
                    .platform_output
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::OutputCommand::OpenUrl(_))),
                expected
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn hosted_page_actions_require_current_owner_row_operation_and_valid_url() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.selected_session = Some("owner".into());
        let action_id = search_activity_ui::page_action("owner", "page:1");
        for (operation, url, owner, expected) in [
            ("open_page", "https://example.com/current", "owner", true),
            ("find_in_page", "https://example.com/updated", "owner", true),
            ("open_page", "file:///etc/passwd", "owner", false),
            ("open_page", "javascript:alert(1)", "owner", false),
            ("search", "https://example.com/search", "owner", false),
            (
                "open_page",
                "https://example.com/stale",
                "other-owner",
                false,
            ),
        ] {
            app.view.selected_session = Some(owner.into());
            app.view.transcript = vec![TranscriptItem::WebSearch {
                id: "page:1".into(),
                item_id: "provider-page".into(),
                action: operation.into(),
                query: None,
                url: Some(url.into()),
                sources: Vec::new(),
                status: ModelStreamStatus::Receiving,
            }];
            let ctx = egui::Context::default();
            ctx.begin_pass(egui::RawInput::default());
            assert!(app.apply_activity_navigation(
                &UiAction::Clicked {
                    id: action_id.clone()
                },
                &ctx
            ));
            let mut output = ctx.end_pass();
            let opened = output
                .platform_output
                .commands
                .iter()
                .find_map(|command| match command {
                    egui::OutputCommand::OpenUrl(open) => Some(open.url.as_str()),
                    _ => None,
                });
            assert_eq!(opened, expected.then_some(url));
            output.textures_delta.clear();
        }
    }

    #[test]
    fn directory_result_actions_cannot_claim_to_open_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.selected_session = Some("owner".into());
        app.view.status = "unchanged".into();
        app.view.transcript.push(TranscriptItem::Tool {
            id: "list".into(),
            name: "list_files".into(),
            arguments: serde_json::json!({"path":"."}),
            output: Some("dir\tsrc".into()),
            is_error: false,
            activity: crate::transcript::ActivityStatus::Completed,
        });
        assert!(app.apply_activity_navigation(
            &UiAction::Clicked {
                id: search_activity_ui::result_action("owner", "list", 0, false),
            },
            &egui::Context::default()
        ));
        assert_eq!(app.view.status, "unchanged");
    }

    #[test]
    fn result_navigation_rejects_reused_tool_call_ids_before_selecting_a_url() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.selected_session = Some("owner".into());
        for url in ["https://example.com/old", "https://example.com/new"] {
            app.view.transcript.push(TranscriptItem::Tool {
                id: "reused".into(),
                name: "web_search".into(),
                arguments: serde_json::json!({"query":"Rust"}),
                output: Some(
                    serde_json::json!({"results":[{"title":"Docs","url":url}]}).to_string(),
                ),
                is_error: false,
                activity: crate::transcript::ActivityStatus::Completed,
            });
        }
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        assert!(app.apply_activity_navigation(
            &UiAction::Clicked {
                id: search_activity_ui::result_action("owner", "reused", 0, false),
            },
            &ctx
        ));
        let mut output = ctx.end_pass();
        assert!(!output
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(command, egui::OutputCommand::OpenUrl(_))));
        output.textures_delta.clear();
    }
}
