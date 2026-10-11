//! Adapters from the application menu to existing workspace commands.
use super::*;

impl AgentsWindow {
    /// The popover backdrop deliberately blocks ordinary header hits. Menu-bar
    /// navigation is the one exception: move directly between adjacent menus.
    pub(super) fn handle_ide_menu_event(&mut self, event: &UiEvent) -> bool {
        if !self.ide_workspace_active() || self.view.ide.menu_open.is_none() {
            return false;
        }
        let categories = ["file", "edit", "selection", "view", "go", "run", "terminal"];
        match event {
            UiEvent::PointerDown(point)
            | UiEvent::PointerMove(point)
            | UiEvent::PointerUp(point) => {
                // Only bypass the menu's backdrop. An actual popup row or
                // floating surface must keep precedence over the header below.
                if self.tree.frame().hit_test(*point).is_some_and(|id| {
                    !id.starts_with("ide_menu:")
                        && id != "ide_menu_dismiss"
                        && !id.starts_with("ide_menu_dismiss:")
                }) {
                    return false;
                }
                let target = self.tree.frame().regions.iter().find_map(|region| {
                    let menu = region.id.strip_prefix("ide_menu:")?;
                    let rect = region.rect;
                    (point.x >= rect.x
                        && point.x < rect.x + rect.width
                        && point.y >= rect.y
                        && point.y < rect.y + rect.height)
                        .then(|| menu.to_owned())
                });
                let Some(target) = target else {
                    return false;
                };
                if matches!(event, UiEvent::PointerDown(_))
                    && (target == "all"
                        || self.view.ide.menu_open.as_deref() == Some(target.as_str()))
                {
                    self.view.ide.menu_open = None;
                } else if !matches!(event, UiEvent::PointerUp(_)) {
                    self.view.ide.menu_open = Some(target);
                }
                true
            }
            UiEvent::Key {
                key: openide_ui::Key::ArrowLeft | openide_ui::Key::ArrowRight,
                ctrl: false,
                shift: false,
            } if self.viewport_width - 2.0 * crate::ide_workspace::OUTER_INSET >= 1200.0 => {
                let current = self
                    .view
                    .ide
                    .menu_open
                    .as_deref()
                    .map(|menu| if menu == "all" { "file" } else { menu });
                let Some(index) = categories.iter().position(|menu| Some(*menu) == current) else {
                    return false;
                };
                let backwards = matches!(
                    event,
                    UiEvent::Key {
                        key: openide_ui::Key::ArrowLeft,
                        ..
                    }
                );
                let next =
                    (index + if backwards { categories.len() - 1 } else { 1 }) % categories.len();
                self.view.ide.menu_open = Some(categories[next].into());
                true
            }
            _ => false,
        }
    }

    pub(super) fn apply_ide_menu_action(&mut self, action: &UiAction, ctx: &egui::Context) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if id == "ide_menu_dismiss"
            || id.strip_prefix("ide_menu_dismiss:").is_some_and(|menu| {
                matches!(
                    menu,
                    "file" | "edit" | "selection" | "view" | "go" | "run" | "terminal"
                )
            })
        {
            self.view.ide.menu_open = None;
            return true;
        }
        if let Some(menu) = id.strip_prefix("ide_menu:") {
            if self.ide_workspace_active()
                && matches!(
                    menu,
                    "all" | "file" | "edit" | "selection" | "view" | "go" | "run" | "terminal"
                )
            {
                self.view.ide.menu_open = if self.view.ide.menu_open.as_deref() == Some(menu)
                    || (menu == "all" && self.view.ide.menu_open.is_some())
                {
                    None
                } else {
                    Some(menu.into())
                };
                self.view.ide.sessions_open = false;
                self.view.ide.compact_panel = None;
                self.view.show_dock_add_menu = false;
                self.view.show_dock_tabs_menu = false;
                self.view.editor_workbench.menu_open = false;
                self.close_composer_floating_controls();
            }
            return true;
        }
        let Some(data) = id.strip_prefix("ide_menu_action:") else {
            return false;
        };
        if !self.ide_workspace_active() || self.view.ide.menu_open.is_none() {
            return true;
        }
        self.view.ide.menu_open = None;
        let Ok((document, target)) = serde_json::from_str::<(Option<(u64, u64)>, String)>(data)
        else {
            return true;
        };
        let editor_action = target.starts_with("editor_")
            || matches!(
                target.as_str(),
                "save" | "cut" | "copy" | "paste" | "select_all"
            );
        if editor_action {
            if target != "save"
                && self
                    .view
                    .document_preview
                    .as_ref()
                    .is_some_and(|view| !view.is_source_visible())
            {
                return true;
            }
            let Some(editor) = self.view.code_editor.as_ref() else {
                return true;
            };
            let Some((tab, _, _, snapshot)) = self.active_editor() else {
                return true;
            };
            if self.view.side_tab != SideTab::FilePreview
                || editor.loading
                || document != Some((tab, snapshot.document_id()))
            {
                return true;
            }
            if matches!(
                target.as_str(),
                "editor_definition" | "editor_references" | "editor_format"
            ) && !self.view.editor_workbench.language_ready
            {
                return true;
            }
            match target.as_str() {
                "save" if !editor.saving => {
                    self.apply_action(
                        UiAction::Clicked {
                            id: format!("code_editor_save:{tab}"),
                        },
                        ctx,
                    );
                    if self
                        .view
                        .document_preview
                        .as_ref()
                        .is_none_or(|view| view.is_source_visible())
                    {
                        self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(tab));
                    }
                }
                "copy" | "cut" => {
                    let mut ranges: Vec<_> = snapshot
                        .selections()
                        .filter(|selection| !selection.is_empty())
                        .map(|selection| selection.range())
                        .collect();
                    ranges.sort_by_key(|range| range.start);
                    // Match the native editor's bounded clipboard policy.
                    if ranges.iter().map(|range| range.len()).sum::<usize>() > 1024 * 1024 {
                        self.view.editor_workbench.error =
                            Some("La selección supera el límite del portapapeles".into());
                        return true;
                    }
                    let text = ranges
                        .into_iter()
                        .map(|range| snapshot.text_range(range))
                        .collect::<Result<Vec<_>, _>>();
                    if let Ok(text) = text {
                        if !text.is_empty() {
                            ctx.copy_text(text.join("\n"));
                            if target == "cut" {
                                self.view.editor_workbench.error = self
                                    .editor_command(
                                        openide_editor::EditorCommand::InsertText(String::new()),
                                        ctx,
                                    )
                                    .err();
                            }
                        }
                    }
                    self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(tab));
                }
                "paste" => {
                    // Remove the menu's focus scope before requesting a scoped,
                    // asynchronous native clipboard read for this editor.
                    self.tree.set_root(crate::view::build_window_view(
                        &self.view,
                        self.viewport_width,
                        self.viewport_height,
                    ));
                    self.tree
                        .layout(Size::new(self.viewport_width, self.viewport_height));
                    let id = crate::code_editor_ui::view_id(tab);
                    if self.tree.focus(&id) {
                        self.request_text_paste(id, ctx);
                    }
                }
                "select_all" => {
                    self.view.editor_workbench.error = self
                        .editor_command(
                            openide_editor::EditorCommand::SetSelection(
                                openide_editor::Selection {
                                    anchor: 0,
                                    head: snapshot.char_count(),
                                },
                            ),
                            ctx,
                        )
                        .err();
                    self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(tab));
                }
                _ if target.starts_with("editor_") => {
                    let return_to_editor = matches!(
                        target.as_str(),
                        "editor_undo"
                            | "editor_redo"
                            | "editor_duplicate"
                            | "editor_move_up"
                            | "editor_move_down"
                            | "editor_select_next"
                            | "editor_select_all_matches"
                            | "editor_cursor_above"
                            | "editor_cursor_below"
                            | "editor_comment"
                            | "editor_format"
                            | "editor_breakpoint"
                            | "editor_wrap"
                            | "editor_copy_path"
                            | "editor_copy_relative"
                    );
                    self.apply_action(UiAction::Clicked { id: target }, ctx);
                    if return_to_editor {
                        self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(tab));
                    }
                }
                _ => {}
            }
            return true;
        }
        if target == "commands" {
            self.open_command_center();
            self.view.command_scope = Scope::Commands;
            self.request_command_search();
            return true;
        }
        if target == "composer_project_browse"
            && (self.view.running || self.view.composer_branches.switching())
        {
            return true;
        }
        if self.apply_debug_menu_action(&target, ctx) {
            return true;
        }
        if let Some(key) = target.strip_prefix("ide_activity:") {
            // The menu reveals the same five activities as the rail, but a
            // second invocation focuses the chosen destination instead of
            // collapsing it. Unknown targets never reach generic dispatch.
            if let Some(activity) = crate::ide_workspace::IdeActivity::from_key(key) {
                self.reveal_ide_activity(activity, ctx);
            }
            return true;
        }
        if matches!(
            target.as_str(),
            "ide_new_file"
                | "composer_project_browse"
                | "ide_quick_open"
                | "ide_explorer_toggle"
                | "ide_agent_toggle"
                | "ide_bottom_toggle"
                | "ide_debug_toggle"
                | "dock_open:review"
                | "dock_open:browser"
                | "dock_open:debugger"
                | "toggle_settings_header"
                | "tour_start:ide"
                | "terminal_toggle"
                | "terminal_new"
                | "terminal_split"
                | "terminal_unsplit"
                | "terminal_panel_maximize"
                | "terminal_settings"
        ) {
            self.apply_action(UiAction::Clicked { id: target }, ctx);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(root: &std::path::Path) -> AgentsWindow {
        let mut app = AgentsWindow::with_data_root(root.to_owned());
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        app.viewport_width = 1440.0;
        app.viewport_height = 900.0;
        app.tree.set_reduced_motion(true);
        app
    }

    fn click(app: &mut AgentsWindow, id: &str) {
        assert!(app.apply_ide_menu_action(
            &UiAction::Clicked { id: id.into() },
            &egui::Context::default()
        ));
    }

    fn command(app: &mut AgentsWindow, scope: Option<(u64, u64)>, target: &str) {
        app.view.ide.menu_open = Some("edit".into());
        click(
            app,
            &format!(
                "ide_menu_action:{}",
                serde_json::to_string(&(scope, target)).unwrap()
            ),
        );
    }

    #[test]
    fn ide_menu_commands_close_before_opening_their_destination() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app(root.path());
        click(&mut app, "ide_menu:file");
        assert_eq!(app.view.ide.menu_open.as_deref(), Some("file"));
        command(&mut app, None, "ide_quick_open");
        assert!(app.view.ide.menu_open.is_none());
        assert!(app.view.show_command_center);
        assert_eq!(app.view.command_scope, Scope::Files);
        app.view.show_command_center = false;
        command(&mut app, None, "commands");
        assert_eq!(app.view.command_scope, Scope::Commands);
    }

    #[test]
    fn ide_menu_activities_reveal_the_sidebar_without_replacing_the_editor_or_collapsing_search() {
        use crate::ide_workspace::IdeActivity;
        let root = tempfile::tempdir().unwrap();
        let mut app = app(root.path());
        let scope = app.dock_scope();
        let file = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: "main.rs".into(),
            },
        );
        app.dock.activate(&scope, file);
        app.view.ide.explorer_open = false;
        command(&mut app, None, "ide_activity:git");
        assert_eq!(app.view.ide.activity, IdeActivity::Git);
        assert!(app.view.ide.explorer_open);
        assert!(app.view.ide.menu_open.is_none());
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, file);
        assert!(
            !app.dock
                .visible_tabs(&scope)
                .iter()
                .any(|tab| matches!(tab.kind, DockTabKind::CodeReview))
        );
        for _ in 0..2 {
            command(&mut app, None, "ide_activity:search");
            assert_eq!(app.view.ide.activity, IdeActivity::Search);
            assert!(app.view.ide.explorer_open);
            assert_eq!(
                app.view.ide.activity_focus.as_deref(),
                Some("ide_search_query")
            );
            assert_eq!(app.dock.active_tab(&scope).unwrap().id, file);
        }
        command(&mut app, None, "ide_activity:unknown");
        assert_eq!(app.view.ide.activity, IdeActivity::Search);
        assert!(app.view.ide.explorer_open);
    }

    #[test]
    fn view_menu_reveals_the_existing_browser_after_opening_the_catalog() {
        use crate::browser_runtime::{BrowserScope, BrowserSnapshot};
        use crate::ide_workspace::IdeActivity;

        let root = tempfile::tempdir().unwrap();
        let mut app = app(root.path());
        app.view.workspace = root.path().display().to_string();
        app.view.selected_session = Some("menu-browser".into());
        let scope = app.dock_scope();
        let browser = app.dock.open(&scope, DockTabKind::Browser);
        app.dock.activate(&scope, browser);
        // The fixture is entirely in memory: no browser process or server is
        // started while the menu goes through the normal host dispatch path.
        let (runtime, _commands) = crate::browser_runtime::tests::fixture(
            BrowserScope {
                workspace: root.path().to_path_buf(),
                session_id: "menu-browser".into(),
            },
            BrowserSnapshot {
                started: true,
                ..Default::default()
            },
        );
        app.browsers.insert(browser.get(), runtime);
        app.sync_dock_view();

        for _ in 0..2 {
            app.select_ide_activity(IdeActivity::Extensions, false);
            assert!(app.view.ide.extensions_open);
            assert_eq!(app.view.ide.extensions_editor_tab, Some(browser.get()));
            click(&mut app, "ide_menu:view");
            click(
                &mut app,
                &format!(
                    "ide_menu_action:{}",
                    serde_json::to_string(&(None::<(u64, u64)>, "dock_open:browser")).unwrap()
                ),
            );
            assert!(!app.view.ide.extensions_open);
            assert!(app.view.ide.menu_open.is_none());
            assert_eq!(app.view.side_tab, SideTab::Browser);
            assert_eq!(app.dock.active_tab(&scope).unwrap().id, browser);
            assert_eq!(app.view.browser.tab_id, Some(browser.get()));
            assert_eq!(
                app.dock
                    .right_tabs(&scope)
                    .iter()
                    .filter(|tab| tab.kind == DockTabKind::Browser)
                    .count(),
                1
            );
            assert_eq!(app.browsers.len(), 1);
            assert!(app.browser_starts.is_empty());
        }
    }

    #[test]
    fn ide_menu_editor_commands_reject_stale_documents_and_hidden_editors() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app(root.path());
        let controller =
            openide_ui::EditorController::new(openide_editor::TextBuffer::new("abc\ndef").unwrap());
        let document = controller.snapshot().unwrap().document_id();
        app.view.code_editor = Some(crate::code_editor_ui::CodeEditorView {
            tab_id: 71,
            path: "main.rs".into(),
            controller: Some(controller.clone()),
            loading: false,
            saving: false,
            error: None,
            notice: None,
            conflict: false,
            recovered: false,
            missing: false,
        });
        app.view.side_tab = SideTab::FilePreview;
        command(&mut app, Some((71, document.wrapping_add(1))), "select_all");
        assert!(controller.snapshot().unwrap().selection().is_empty());
        app.view.side_tab = SideTab::CodeReview;
        command(&mut app, Some((71, document)), "select_all");
        assert!(controller.snapshot().unwrap().selection().is_empty());
        app.view.side_tab = SideTab::FilePreview;
        command(&mut app, Some((71, document)), "select_all");
        assert_eq!(controller.snapshot().unwrap().selection().range(), 0..7);
        controller
            .execute(openide_editor::EditorCommand::InsertText(
                "replacement".into(),
            ))
            .unwrap();
        app.view.editor_workbench.document_id = Some(document);
        // IDE actions belong to the tab titlebar, above the breadcrumb/editor.
        let editor = app.view.code_editor.as_ref().unwrap();
        app.tree.set_root(Element::column(vec![
            Element::row(crate::code_editor_ui::titlebar_actions(
                editor,
                &app.view.editor_workbench,
                app.view.keymap_profile,
            ))
            .size(None, Some(crate::dock_tab_ui::HEADER_HEIGHT)),
            crate::code_editor_ui::build_ide(editor, &app.view.editor_workbench, 800.0, app.view.keymap_profile, app.view.vim_mode).flex(1.0),
        ]));
        app.tree.layout(Size::new(800.0, 600.0));
        assert!(app.tree.focus("editor_menu"));
        command(&mut app, Some((71, document)), "editor_undo");
        assert_eq!(
            app.editor_services.focus_next.as_deref(),
            Some("code_editor:71")
        );
        app.poll_editor_workbench(&egui::Context::default());
        assert_eq!(app.tree.focused_id(), Some("code_editor:71"));
        app.tree.handle_event(UiEvent::Text("typed".into()));
        let snapshot = controller.snapshot().unwrap();
        assert_eq!(
            snapshot.text_range(0..snapshot.char_count()).unwrap(),
            "typed"
        );
    }

    #[test]
    fn ide_menu_header_switching_never_overrides_a_floating_control() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app(root.path());
        app.view.ide.menu_open = Some("view".into());
        app.tree.set_root(openide_ui::Element::overlay(
            openide_ui::Element::button("ide_menu:terminal", "Terminal"),
            openide_ui::Element::button("popup_action", "Acción del menú"),
        ));
        let frame = app.tree.layout(Size::new(220.0, 30.0));
        let point = openide_ui::Point::new(12.0, 12.0);
        assert_eq!(frame.hit_test(point), Some("popup_action"));
        for event in [
            UiEvent::PointerMove(point),
            UiEvent::PointerDown(point),
            UiEvent::PointerUp(point),
        ] {
            assert!(!app.handle_ide_menu_event(&event));
            assert_eq!(app.view.ide.menu_open.as_deref(), Some("view"));
        }
    }

    #[test]
    fn ide_menu_switches_categories_through_the_backdrop_and_cycles_with_arrows() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app(root.path());
        click(&mut app, "ide_menu:file");
        app.tree
            .set_root(crate::view::build_window_view(&app.view, 1440.0, 900.0));
        let frame = app.tree.layout(Size::new(1440.0, 900.0));
        let rect = frame
            .regions
            .iter()
            .find(|region| region.id == "ide_menu:edit")
            .unwrap()
            .rect;
        let point = openide_ui::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        assert!(app.handle_ide_menu_event(&UiEvent::PointerDown(point)));
        assert_eq!(app.view.ide.menu_open.as_deref(), Some("edit"));
        assert!(app.handle_ide_menu_event(&UiEvent::PointerUp(point)));
        assert_eq!(app.view.ide.menu_open.as_deref(), Some("edit"));
        assert!(app.handle_ide_menu_event(&UiEvent::Key {
            key: openide_ui::Key::ArrowRight,
            ctrl: false,
            shift: false
        }));
        assert_eq!(app.view.ide.menu_open.as_deref(), Some("selection"));
        assert!(app.handle_ide_menu_event(&UiEvent::Key {
            key: openide_ui::Key::ArrowRight,
            ctrl: false,
            shift: false,
        }));
        app.tree
            .set_root(crate::view::build_window_view(&app.view, 1440.0, 900.0));
        app.tree.layout(Size::new(1440.0, 900.0));
        let actions = app.tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::Enter,
            ctrl: false,
            shift: false,
        });
        assert!(
            actions.iter().any(
                |action| matches!(action, UiAction::Clicked { id } if id.ends_with("\"commands\"]"))
            ),
            "{actions:?}"
        );
        for action in actions {
            app.apply_ide_menu_action(&action, &egui::Context::default());
        }
        assert!(app.view.show_command_center);
        assert_eq!(app.view.command_scope, Scope::Commands);
        app.view.show_command_center = false;
        app.viewport_width = 640.0;
        app.view.ide.menu_open = Some("all".into());
        assert!(!app.handle_ide_menu_event(&UiEvent::Key {
            key: openide_ui::Key::ArrowRight,
            ctrl: false,
            shift: false
        }));
    }
}
