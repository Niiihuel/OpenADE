//! Activity navigation changes presentation only. Documents, debugger sessions
//! and the agent remain owned by their existing workspace services.
use super::*;
use crate::ide_workspace::{IdeActivity, IdePanel};

impl AgentsWindow {
    pub(super) fn move_git_form_to_ide_sidebar(&mut self) {
        // The sidebar and conversation reuse one controlled Git form. Finish
        // the old presentation immediately so its closing animation cannot
        // duplicate input/action IDs alongside the newly visible sidebar.
        self.close_environment();
        self.environment_motion = None;
        self.view.environment_presentation_progress = None;
    }

    /// Startup and project switches must populate the selected activity just
    /// like an explicit rail click, without displaying another project's skills.
    pub(super) fn refresh_ide_activity_workspace(&mut self) -> bool {
        if !self.ide_workspace_active()
            || self.view.ide.activity != IdeActivity::Extensions
            || self.view.ide.extensions_workspace.as_deref() == Some(self.view.workspace.as_str())
        {
            return false;
        }
        self.view.ide.extensions_workspace = Some(self.view.workspace.clone());
        self.reset_marketplace_workspace();
        self.marketplace_operation(MarketplaceOperation::Load);
        true
    }

    pub(super) fn select_ide_activity(&mut self, activity: IdeActivity, toggle: bool) {
        let visible = crate::ide_workspace::nominal_layout(&self.view, self.viewport_width).left
            > 0.0
            || self.view.ide.compact_panel == Some(IdePanel::Explorer);
        if toggle && self.view.ide.activity == activity && visible {
            self.view.ide.explorer_open = false;
            self.view.ide.compact_panel = None;
        } else {
            self.view.ide.activity = activity;
            self.view.ide.explorer_open = true;
            self.view.ide.compact_panel =
                (crate::ide_workspace::nominal_layout(&self.view, self.viewport_width).left <= 0.0)
                    .then_some(IdePanel::Explorer);
        }
        if activity == IdeActivity::Extensions && !self.view.ide.extensions_open {
            self.view.ide.extensions_editor_tab = self
                .dock
                .active_tab(&self.dock_scope())
                .map(|tab| tab.id.get());
        }
        self.view.ide.extensions_open = activity == IdeActivity::Extensions;
        if activity == IdeActivity::Git && self.view.ide.explorer_open {
            self.move_git_form_to_ide_sidebar();
        }
        self.view.ide.menu_open = None;
        self.view.ide.git_actions_open = false;
        self.view.ide.sessions_open = false;
        self.close_composer_floating_controls();
        self.view.show_dock_add_menu = false;
        self.view.show_dock_tabs_menu = false;
        self.view.ide.activity_focus = if self.view.ide.explorer_open {
            match activity {
                IdeActivity::Search => Some("ide_search_query".into()),
                IdeActivity::Extensions => Some("ide_extensions_search".into()),
                _ => None,
            }
        } else {
            None
        };
        self.persist_ide_workspace();
    }

    /// Keyboard navigation reveals/focuses an activity without toggling it
    /// closed when it is already selected.
    pub(super) fn reveal_ide_activity(&mut self, activity: IdeActivity, ctx: &egui::Context) {
        self.select_ide_activity(activity, false);
        self.refresh_revealed_ide_activity(activity, ctx);
        ctx.request_repaint();
    }

    fn refresh_revealed_ide_activity(&mut self, activity: IdeActivity, ctx: &egui::Context) {
        if !self.view.ide.explorer_open {
            return;
        }
        match activity {
            IdeActivity::Files => self.refresh_file_rows(),
            IdeActivity::Search => {}
            IdeActivity::Git => self.request_git_review(PathBuf::from(&self.view.workspace)),
            IdeActivity::Debug => {
                self.apply_debug_action(
                    &UiAction::Clicked {
                        id: "dock_open:debugger".into(),
                    },
                    ctx,
                );
            }
            IdeActivity::Extensions => {
                if !self.refresh_ide_activity_workspace() {
                    self.marketplace_operation(MarketplaceOperation::Load);
                }
            }
        }
    }

    pub(super) fn apply_ide_activity_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        if !self.ide_workspace_active() {
            return false;
        }
        if let UiAction::InputChanged { id, value } = action {
            if id == "ide_extensions_search" {
                self.set_marketplace_query(value.clone());
                return true;
            }
        }
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if id.starts_with("ide_marketplace_tab:") {
            self.handle_marketplace_action(id);
            return true;
        }
        if self.view.ide.activity == IdeActivity::Git {
            use crate::environment_git_ui::EnvironmentGitPanel;
            if id == "ide_git_more" || id == "ide_git_more_dismiss" {
                self.view.ide.git_actions_open =
                    id == "ide_git_more" && !self.view.ide.git_actions_open;
                return true;
            }
            if id.starts_with("environment_git_") {
                self.view.ide.git_actions_open = false;
            }
            if id.starts_with("environment_git_open_") {
                self.move_git_form_to_ide_sidebar();
            }
            if matches!(id.as_str(), "environment_close" | "dock_open:review")
                && self.view.environment.workflow.panel != EnvironmentGitPanel::Overview
            {
                self.view.environment.workflow.panel = EnvironmentGitPanel::Overview;
                self.view.environment.workflow.remote_menu_open = false;
                self.view.environment.workflow.compare_menu_open = false;
                return true;
            }
        }
        // Reopening the same resource is intentional navigation even when the dock
        // target has not changed. The owning handlers still validate and apply
        // their actions; this hook only reveals the center presentation.
        let file_row = id
            .strip_prefix("file_row:")
            .and_then(|raw| raw.parse::<usize>().ok())
            .and_then(|index| self.view.file_rows.get(index))
            .is_some_and(|row| !row.is_dir);
        let dock_tab = id
            .strip_prefix("dock_tab:")
            .or_else(|| id.strip_prefix("dock_tab_menu:"))
            .and_then(|raw| raw.parse::<u64>().ok())
            .is_some_and(|id| self.view.dock_tabs.iter().any(|tab| tab.id == id));
        let tool_file = id.strip_prefix("tool_navigate:")
            .and_then(|raw| serde_json::from_str::<(String, String)>(raw).ok())
            .filter(|(owner, call)| self.tool_navigation_is_unique(owner, call))
            .is_some_and(|(_, call)| self.view.transcript.iter().any(|item| {
                matches!(item, TranscriptItem::Tool { id, name, arguments, .. }
                    if id == &call && matches!(crate::tool_presentation::navigation_target(name, arguments),
                        Some(crate::tool_presentation::ToolNavigationTarget::File(_))))
            }));
        if file_row
            || dock_tab
            || tool_file
            || id == "dock_open:browser"
            || id.starts_with("file_activity_open:")
            || id.starts_with("chat_diff_file:")
            || id.starts_with("environment_source_file:")
        {
            self.view.ide.extensions_open = false;
        }
        if let Some(key) = id.strip_prefix("ide_activity:") {
            let Some(activity) = IdeActivity::from_key(key) else {
                return true;
            };
            self.select_ide_activity(activity, true);
            self.refresh_revealed_ide_activity(activity, ctx);
            return true;
        }
        if id == "ide_extensions_catalog" {
            self.view.ide.extensions_editor_tab = self
                .dock
                .active_tab(&self.dock_scope())
                .map(|tab| tab.id.get());
            self.view.ide.extensions_open = true;
            self.view.marketplace.selected_item_id = None;
            self.view.ide.compact_panel = None;
            return true;
        }
        if id == "ide_extensions_close" {
            self.view.ide.extensions_open = false;
            return true;
        }
        if let Some(item_id) = id.strip_prefix("ide_extension:") {
            if self.view.marketplace.find_item(item_id).is_some() {
                self.view.marketplace.selected_item_id = Some(item_id.to_owned());
                self.view.marketplace.menu_item = None;
                self.view.ide.extensions_editor_tab = self
                    .dock
                    .active_tab(&self.dock_scope())
                    .map(|tab| tab.id.get());
                self.view.ide.extensions_open = true;
                self.view.ide.compact_panel = None;
            }
            return true;
        }
        if self.view.ide.activity == IdeActivity::Git
            && (id.starts_with("review_file_open:") || id.starts_with("review_untracked_open:"))
        {
            // The existing action below loads and validates the selected diff.
            // Activate its center tab first; the navigator remains independent.
            let scope = self.dock_scope();
            let workspace_scope = DockScope::new(scope.workspace_id.clone(), DockOwner::Workspace);
            let tab = self.dock.open(&workspace_scope, DockTabKind::CodeReview);
            self.dock.activate(&scope, tab);
            self.dock.set_collapsed(&scope, false);
            self.view.ide.extensions_open = false;
            self.view.ide.compact_panel = None;
            self.sync_dock_view();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide_workspace::WindowMode;

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.ide.mode = WindowMode::Ide;
        app.viewport_width = 1440.0;
        app.viewport_height = 900.0;
        (root, app)
    }

    fn click(id: &str) -> UiAction {
        UiAction::Clicked { id: id.into() }
    }

    #[test]
    fn ide_activity_navigation_preserves_documents_and_collapses_only_the_active_sidebar() {
        let (_root, mut app) = fixture();
        let scope = app.dock_scope();
        let document = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: "main.rs".into(),
            },
        );
        app.dock.activate(&scope, document);
        let ctx = egui::Context::default();
        assert!(app.apply_ide_activity_action(&click("ide_activity:git"), &ctx));
        assert_eq!(app.view.ide.activity, IdeActivity::Git);
        assert!(app.view.ide.explorer_open);
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, document);
        app.apply_ide_activity_action(&click("ide_activity:git"), &ctx);
        assert!(!app.view.ide.explorer_open);
        app.apply_ide_activity_action(&click("ide_activity:search"), &ctx);
        assert!(app.view.ide.explorer_open);
        assert_eq!(app.view.ide.activity, IdeActivity::Search);
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, document);
        assert!(!app.view.debug_open);
    }

    #[test]
    fn ide_activity_compact_navigation_reuses_the_drawer_and_catalog_stays_in_workspace() {
        let (_root, mut app) = fixture();
        app.viewport_width = 320.0;
        let ctx = egui::Context::default();
        app.apply_ide_activity_action(&click("ide_activity:extensions"), &ctx);
        assert_eq!(app.view.ide.compact_panel, Some(IdePanel::Explorer));
        assert!(app.view.ide.extensions_open);
        assert_eq!(app.view.main_page, MainPage::Workspace);
        app.apply_ide_activity_action(&click("ide_extensions_catalog"), &ctx);
        assert!(app.view.ide.compact_panel.is_none());
        app.apply_ide_activity_action(&click("ide_extensions_close"), &ctx);
        assert!(!app.view.ide.extensions_open);
        app.apply_ide_activity_action(&click("ide_activity:files"), &ctx);
        assert_eq!(app.view.ide.compact_panel, Some(IdePanel::Explorer));
        app.apply_ide_activity_action(&click("ide_activity:files"), &ctx);
        assert!(!app.view.ide.explorer_open);
        assert!(app.view.ide.compact_panel.is_none());
    }

    #[test]
    fn a_sidebar_row_opens_its_detail_and_the_home_button_returns_to_the_landing() {
        let (_root, mut app) = fixture();
        let ctx = egui::Context::default();
        app.view
            .marketplace
            .snapshot
            .items
            .push(crate::marketplace::MarketplaceItem {
                id: "source:review".into(),
                name: "review".into(),
                description: "Review changes".into(),
                license: None,
                source_name: "Local".into(),
                source_kind: SourceKind::Local,
                installed_scope: None,
                enabled: false,
                source_available: true,
            });
        app.apply_ide_activity_action(&click("ide_extension:missing"), &ctx);
        assert!(app.view.marketplace.selected_item_id.is_none());
        assert!(!app.view.ide.extensions_open);
        app.view.marketplace.menu_item = Some("row:source:review".into());
        app.apply_ide_activity_action(&click("ide_extension:source:review"), &ctx);
        assert_eq!(
            app.view.marketplace.selected_item_id.as_deref(),
            Some("source:review")
        );
        assert!(app.view.ide.extensions_open);
        assert!(app.view.marketplace.menu_item.is_none());
        app.apply_ide_activity_action(&click("ide_extensions_catalog"), &ctx);
        assert!(app.view.marketplace.selected_item_id.is_none());
        assert!(app.view.ide.extensions_open, "the landing page, not the editor");
    }

    #[test]
    fn repeated_activity_clicks_toggle_the_destination_during_sidebar_motion() {
        let (_root, mut app) = fixture();
        app.view.ide.activity = IdeActivity::Search;
        app.view.ide.explorer_open = false;
        app.view.left_presentation_width = Some(120.0);
        let ctx = egui::Context::default();
        assert!(app.apply_ide_activity_action(&click("ide_activity:search"), &ctx));
        assert!(app.view.ide.explorer_open);
        assert!(app.view.ide.compact_panel.is_none());
        assert_eq!(
            app.view.ide.activity_focus.as_deref(),
            Some("ide_search_query")
        );
        app.view.left_presentation_width = Some(0.0);
        assert!(app.apply_ide_activity_action(&click("ide_activity:search"), &ctx));
        assert!(!app.view.ide.explorer_open);
        assert!(app.view.ide.compact_panel.is_none());
        assert!(app.view.ide.activity_focus.is_none());
        app.reveal_ide_activity(IdeActivity::Search, &ctx);
        assert!(app.view.ide.explorer_open);
        assert!(app.view.ide.compact_panel.is_none());
        assert_eq!(app.view.ide.explorer_width, 240.0);
    }

    #[test]
    fn marketplace_search_stays_synchronized_between_sidebar_and_catalog() {
        let (_root, mut app) = fixture();
        let ctx = egui::Context::default();
        let inputs = || {
            openide_ui::Element::column(vec![
                openide_ui::Element::text_input("marketplace_search", "", ""),
                openide_ui::Element::text_input("ide_extensions_search", "", ""),
            ])
        };
        app.tree = UiTree::new(inputs());
        app.tree.layout(Size::new(600.0, 200.0));
        assert!(app.tree.focus("ide_extensions_search"));
        for action in app
            .tree
            .handle_event(UiEvent::Text("frontend-design".into()))
        {
            app.apply_action(action, &ctx);
        }
        assert_eq!(app.view.marketplace.query, "frontend-design");
        assert_eq!(
            app.tree.input_value("marketplace_search"),
            Some("frontend-design")
        );
        assert_eq!(
            app.tree.input_value("ide_extensions_search"),
            Some("frontend-design")
        );
        // Reopening the catalog keeps both retained controllers in sync.
        app.tree.set_root(inputs());
        app.tree.layout(Size::new(600.0, 200.0));
        assert!(app.tree.focus("marketplace_search"));
        app.tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::A,
            ctrl: true,
            shift: false,
        });
        for action in app.tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::Backspace,
            ctrl: false,
            shift: false,
        }) {
            app.apply_action(action, &ctx);
        }
        assert_eq!(app.view.marketplace.query, "");
        assert_eq!(app.tree.input_value("marketplace_search"), Some(""));
        assert_eq!(app.tree.input_value("ide_extensions_search"), Some(""));
        for action in app.tree.handle_event(UiEvent::Text("react".into())) {
            app.apply_action(action, &ctx);
        }
        assert_eq!(app.view.marketplace.query, "react");
        assert_eq!(app.tree.input_value("ide_extensions_search"), Some("react"));
    }

    #[test]
    fn ide_catalog_survives_background_sync_but_document_navigation_reveals_the_editor() {
        let (_root, mut app) = fixture();
        let scope = app.dock_scope();
        let original = app.dock.open(&scope, DockTabKind::CodeReview);
        app.dock.activate(&scope, original);
        app.select_ide_activity(IdeActivity::Extensions, false);
        app.sync_code_editor();
        assert!(
            app.view.ide.extensions_open,
            "a background sync must not close the catalog"
        );
        let next = app.dock.open(&scope, DockTabKind::Context);
        app.dock.activate(&scope, next);
        app.sync_code_editor();
        assert!(!app.view.ide.extensions_open);

        app.select_ide_activity(IdeActivity::Extensions, false);
        app.view.file_rows = vec![crate::view::FileRowView {
            name: "main.rs".into(),
            depth: 0,
            is_dir: false,
            expanded: false,
            can_load_more: false,
        }];
        assert!(!app.apply_ide_activity_action(&click("file_row:0"), &egui::Context::default()));
        assert!(
            !app.view.ide.extensions_open,
            "even reopening the same file reveals the editor"
        );
    }

    #[test]
    fn ide_git_reveal_transfers_the_live_form_without_duplicate_controls_or_losing_its_draft() {
        use crate::environment_git_ui::EnvironmentGitPanel;
        let (_root, mut app) = fixture();
        app.view.selected_session = Some("current".into());
        app.view.environment.open = true;
        app.view.environment_presentation_progress = Some(0.5);
        app.view.environment.workflow.panel = EnvironmentGitPanel::Commit;
        app.view.environment.workflow.commit_message = "Mantener este borrador".into();
        app.select_ide_activity(IdeActivity::Git, false);
        assert!(!app.view.environment.open);
        assert!(app.view.environment_presentation_progress.is_none());
        assert_eq!(
            app.view.environment.workflow.panel,
            EnvironmentGitPanel::Commit
        );
        assert_eq!(
            app.view.environment.workflow.commit_message,
            "Mantener este borrador"
        );
        assert!(!app.update_environment_motion(Instant::now()));
        app.tree
            .set_root(crate::view::build_window_view(&app.view, 1440.0, 900.0));
        let frame = app.tree.layout(Size::new(1440.0, 900.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "chat_environment"));
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id == "environment_git_back")
                .count(),
            1
        );
    }
}
