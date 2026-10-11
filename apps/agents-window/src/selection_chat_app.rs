//! Sends the code editor's selection to the agent of the selected task: a
//! chip in the composer, or a reference typed into an agent CLI's prompt.
use super::*;
use crate::selection_chat;
use crate::view::AttachedSelection;

/// How long a selection rests before its control shows.
const IDLE: Duration = Duration::from_millis(350);
/// All the selections of one message. It keeps a message, and the draft of
/// one sent to a running turn, well inside their 32 KiB.
const MAX_SELECTION_BYTES: usize = 16 * 1024;

impl AgentsWindow {
    /// Where the control belongs now, from the last layout: over a selection
    /// that rested in the focused editor, with its moving end in view.
    pub(super) fn selection_control_target(&mut self, ctx: &egui::Context) -> Option<openide_ui::Point> {
        let shown = self.view.editor_workbench.selection_control;
        // Pressing the control takes focus from the editor; it stays put
        // until the click lands.
        if shown.is_some() && self.tree.hovered_id() == Some(selection_chat::ACTION) {
            return shown;
        }
        let snapshot = self
            .active_editor()
            .filter(|(tab, ..)| {
                self.tree.focused_id() == Some(crate::code_editor_ui::view_id(*tab).as_str())
            })
            .map(|(_, _, _, snapshot)| snapshot)
            .filter(|snapshot| !snapshot.selection().is_empty());
        let selected = snapshot
            .as_ref()
            .map(|snapshot| (snapshot.document_id(), snapshot.selection()));
        let seen = &mut self.editor_services.selection_seen;
        if selected != seen.map(|(key, _)| key) {
            *seen = selected.map(|key| (key, Instant::now()));
        }
        let ((_, selection), since) = (*seen)?;
        let row = |offset| snapshot.as_ref()?.position_of(offset).ok().map(|at| at.row as isize);
        let rows = row(selection.head)? - row(selection.anchor)?;
        // No rectangles while a menu is open, the end is scrolled out of view
        // or Vim is outside insert mode.
        let (body, caret) = self.tree.editor_ime_rects()?;
        if ctx.input(|input| input.pointer.any_down()) {
            return None;
        }
        let waited = since.elapsed();
        if waited < IDLE {
            ctx.request_repaint_after(IDLE - waited);
            return None;
        }
        Some(selection_chat::place(body, caret, rows))
    }

    pub(super) fn sync_selection_control(&mut self, ctx: &egui::Context) -> bool {
        let target = self.selection_control_target(ctx);
        let changed = target != self.view.editor_workbench.selection_control;
        self.view.editor_workbench.selection_control = target;
        changed
    }

    /// The selection of the active editor, or the caret's line when nothing
    /// is selected, goes to the agent of the selected task.
    pub(super) fn add_editor_selection_to_chat(&mut self) {
        let Some((_, path, _, snapshot)) = self.active_editor() else {
            return;
        };
        let range = snapshot.selection().range();
        let row = |offset| snapshot.position_of(offset).map(|position| position.row);
        let (Ok(first), Ok(mut last)) = (row(range.start), row(range.end)) else {
            return;
        };
        let text = if range.is_empty() {
            snapshot.line_text(first)
        } else {
            // A selection that ends where a line starts does not hold that line.
            if last > first
                && snapshot
                    .line_range(last)
                    .is_ok_and(|line| line.start == range.end)
            {
                last -= 1;
            }
            snapshot.text_range(range)
        };
        let Ok(text) = text else {
            return;
        };
        let selection = AttachedSelection {
            start_line: first as u32 + 1,
            end_line: last as u32 + 1,
            text,
        };
        let path = path.replace('\\', "/");
        self.view.editor_workbench.selection_control = None;
        if crate::view::cli_task(&self.view) {
            self.send_selection_to_cli(&path, &selection, snapshot.is_dirty());
        } else {
            self.attach_selection(path, selection);
        }
    }

    fn attach_selection(&mut self, path: String, selection: AttachedSelection) {
        let attached = &self.view.composer_attachments;
        let held: usize = attached
            .iter()
            .filter_map(|existing| existing.selection.as_ref())
            .map(|existing| existing.text.len())
            .sum();
        self.view.status = if attached.len() >= MAX_COMPOSER_ATTACHMENTS {
            format!("Máximo de {MAX_COMPOSER_ATTACHMENTS} adjuntos")
        } else if held + selection.text.len() > MAX_SELECTION_BYTES {
            "La selección supera 16 KiB por mensaje; adjunta el archivo".to_owned()
        } else if attached
            .iter()
            .any(|existing| existing.path == path && existing.selection.as_ref() == Some(&selection))
        {
            "La selección ya está adjunta".to_owned()
        } else {
            let label = path.rsplit('/').next().unwrap_or(&path).to_owned();
            let status = format!("Selección adjunta: {label} L{}", selection.lines());
            self.view.composer_attachments.push(ComposerAttachmentView {
                path,
                label,
                is_dir: false,
                selection: Some(selection),
            });
            let key = self.draft_key();
            self.attachment_drafts
                .insert(key, self.view.composer_attachments.clone());
            self.draft_dirty_at = Some(Instant::now());
            self.save_draft();
            status
        };
        self.reveal_ide_agent();
        self.prompt_focus_pending = true;
    }

    /// An agent CLI reads the file itself: it gets the path and the lines,
    /// typed into its prompt and not submitted.
    fn send_selection_to_cli(&mut self, path: &str, selection: &AttachedSelection, dirty: bool) {
        let Some((agent, _)) = self
            .view
            .selected_backend
            .strip_prefix(crate::view::CLI_BACKEND)
            .and_then(crate::agent_cli_definitions::profile)
        else {
            return;
        };
        let scope = self.dock_scope();
        let terminal = self
            .dock
            .terminal_tabs(&scope)
            .iter()
            .map(|tab| tab.id.get())
            .find(|id| self.terminal_agent(*id).is_some_and(|found| found.id == agent.id));
        let Some(session) = terminal.and_then(|id| self.terminals.get_mut(&id)) else {
            self.view.status = format!("{} no está en ejecución en esta tarea", agent.label);
            return;
        };
        let reference =
            selection_chat::cli_reference(&agent.selection_reference, path, &selection.lines());
        self.view.status = match session.paste(&reference) {
            // The CLI sees the file on disk, not the editor's unsaved text.
            Ok(()) if dirty => format!(
                "Selección enviada a {}: {}. Guarda el archivo para que lea esas líneas.",
                agent.label,
                reference.trim_end()
            ),
            Ok(()) => format!("Selección enviada a {}: {}", agent.label, reference.trim_end()),
            Err(error) => format!("No se pudo escribir en {}: {error}", agent.label),
        };
        self.reveal_ide_agent();
        self.terminal_focus_pending = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_editor::Selection;
    use openide_ui::{DrawCommand, Frame, HitKind, Point, Rect, UiEvent};

    const SOURCE: &str = "fn main() {\n    let saludo = \"hola\";\n    println!(\"{saludo}\");\n}\n";

    /// A project with `src/main.rs` open and focused in the editor.
    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/main.rs"), SOURCE).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.canonicalize().unwrap().to_string_lossy().into_owned();
        app.view.workspace_draft = app.view.workspace.clone();
        app.view.project_branch = None;
        app.view.sidebar_collapsed = true;
        app.view.show_settings = false;
        app.view.main_page = MainPage::Workspace;
        let scope = app.dock_scope();
        let tab = app
            .dock
            .open(&scope, DockTabKind::FilePreview { path: "src/main.rs".into() });
        assert!(app.dock.activate(&scope, tab));
        app.dock.set_collapsed(&scope, false);
        app.dock.set_width_px(&scope.workspace_id, 520.0);
        app.sync_dock_view();
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.view.code_editor.as_ref().is_none_or(|view| view.loading) {
            app.poll_code_editor(&ctx);
            assert!(Instant::now() < deadline, "the editor never loaded");
            std::thread::sleep(Duration::from_millis(2));
        }
        frame(&mut app);
        let editor = crate::code_editor_ui::view_id(tab.get());
        assert!(app.tree.focus(&editor));
        (root, app)
    }

    const VIEWPORT: Size = Size::new(1100.0, 720.0);

    fn frame(app: &mut AgentsWindow) -> Frame {
        app.viewport_width = VIEWPORT.width;
        app.tree.set_root(crate::view::build_window_view(
            &app.view,
            VIEWPORT.width,
            VIEWPORT.height,
        ));
        app.tree.set_reduced_motion(true);
        app.tree.layout(VIEWPORT)
    }

    fn select(app: &mut AgentsWindow, anchor: usize, head: usize) {
        let controller = app.view.code_editor.as_ref().unwrap().controller.clone().unwrap();
        controller
            .shared_buffer()
            .lock()
            .unwrap()
            .set_selection(Selection { anchor, head })
            .unwrap();
    }

    /// The control as it is once the selection has rested.
    fn rested(app: &mut AgentsWindow) -> Option<Rect> {
        let ctx = egui::Context::default();
        frame(app);
        app.sync_selection_control(&ctx);
        if let Some((_, since)) = &mut app.editor_services.selection_seen {
            *since -= IDLE;
        }
        app.sync_selection_control(&ctx);
        frame(app)
            .regions
            .iter()
            .find(|region| region.id == selection_chat::ACTION && region.kind == HitKind::Button)
            .map(|region| region.rect)
    }

    fn click(app: &mut AgentsWindow, id: &str) {
        let rect = frame(app)
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == HitKind::Button)
            .unwrap_or_else(|| panic!("no {id}"))
            .rect;
        let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        app.tree.handle_event(UiEvent::PointerMove(point));
        app.tree.handle_event(UiEvent::PointerDown(point));
        // The press took focus from the editor: the control must still be there.
        app.sync_selection_control(&egui::Context::default());
        frame(app);
        for action in app.tree.handle_event(UiEvent::PointerUp(point)) {
            app.apply_action(action, &egui::Context::default());
        }
    }

    // Lines 2 and 3 of SOURCE, up to the last line break.
    const LINES_2_3: (usize, usize) = (12, 62);

    #[test]
    fn control_shows_over_a_rested_selection_only_and_leaves_with_it() {
        let (_root, mut app) = fixture();
        assert!(rested(&mut app).is_none(), "nothing is selected");

        select(&mut app, LINES_2_3.0, LINES_2_3.1);
        let ctx = egui::Context::default();
        frame(&mut app);
        assert!(!app.sync_selection_control(&ctx), "it waits for the selection to rest");
        let control = rested(&mut app).expect("control over the selection");
        let editor = app
            .tree
            .frame()
            .regions
            .iter()
            .find(|region| region.id.starts_with("code_editor:"))
            .unwrap()
            .rect;
        assert!(control.x >= editor.x && control.x + control.width <= editor.x + editor.width);
        // Showing it leaves the keyboard in the editor.
        assert!(app.tree.focused_id().unwrap().starts_with("code_editor:"));
        // Selected downwards: it sits under the caret, clear of every selected line.
        let (_, caret) = app.tree.editor_ime_rects().unwrap();
        assert!(control.y >= caret.y + caret.height);

        // Selected upwards with no room over the first line: under the last
        // one, two lines down, and so never over the selected text.
        select(&mut app, LINES_2_3.1, LINES_2_3.0);
        let control = rested(&mut app).unwrap();
        let (_, caret) = app.tree.editor_ime_rects().unwrap();
        assert!(control.y >= caret.y + 2.0 * caret.height);
        // Within the last line there is room over it.
        select(&mut app, 60, 58);
        let control = rested(&mut app).unwrap();
        let (_, caret) = app.tree.editor_ime_rects().unwrap();
        assert!(control.y + control.height <= caret.y);

        // Typing replaces the selection, and the control goes.
        for action in app.tree.handle_event(UiEvent::Text("x".into())) {
            app.apply_action(action, &ctx);
        }
        assert!(rested(&mut app).is_none());

        // A selection in an editor without the keyboard has no control.
        select(&mut app, 0, 5);
        assert!(rested(&mut app).is_some());
        assert!(app.tree.focus("prompt"));
        assert!(rested(&mut app).is_none());
    }

    #[test]
    fn the_control_adds_a_removable_chip_with_the_path_and_the_lines() {
        let (_root, mut app) = fixture();
        select(&mut app, LINES_2_3.0, LINES_2_3.1);
        assert!(rested(&mut app).is_some());
        click(&mut app, selection_chat::ACTION);

        let expected = ComposerAttachmentView {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
            is_dir: false,
            selection: Some(AttachedSelection {
                start_line: 2,
                end_line: 3,
                text: "    let saludo = \"hola\";\n    println!(\"{saludo}\");".into(),
            }),
        };
        assert_eq!(app.view.composer_attachments, vec![expected.clone()]);
        assert!(app.view.editor_workbench.selection_control.is_none());
        assert!(app.prompt_focus_pending);
        let painted = frame(&mut app);
        let shows = |text: &str| {
            painted.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text: shown, .. } if shown == text),
            )
        };
        assert!(shows("main.rs") && shows("L2–L3"), "chip with file and lines");

        // The same selection twice is one chip; another range is another.
        app.add_editor_selection_to_chat();
        assert_eq!(app.view.composer_attachments.len(), 1);
        select(&mut app, 0, 3);
        app.add_editor_selection_to_chat();
        assert_eq!(app.view.composer_attachments[1].selection.as_ref().unwrap().lines(), "1");
        // Nothing selected: the caret's line.
        select(&mut app, 63, 63);
        app.add_editor_selection_to_chat();
        let line = app.view.composer_attachments[2].selection.clone().unwrap();
        assert_eq!((line.lines().as_str(), line.text.trim_end()), ("4", "}"));

        click(&mut app, "composer_attachment_remove:0");
        assert_eq!(app.view.composer_attachments.len(), 2);
        assert!(!app.view.composer_attachments.contains(&expected));
    }

    #[test]
    fn ctrl_l_in_the_editor_adds_the_selection() {
        let (_root, mut app) = fixture();
        select(&mut app, LINES_2_3.0, LINES_2_3.1);
        let event = UiEvent::Key { key: openide_ui::Key::L, shift: false, ctrl: true };
        assert!(app.handle_editor_workbench_key(&event, &egui::Context::default()));
        assert_eq!(app.view.composer_attachments.len(), 1);
    }

    #[test]
    fn the_harness_prompt_carries_path_lines_and_the_text_as_it_was() {
        let (_root, mut app) = fixture();
        select(&mut app, LINES_2_3.0, LINES_2_3.1);
        app.add_editor_selection_to_chat();
        // The file changes after the selection was attached.
        select(&mut app, LINES_2_3.0, LINES_2_3.1);
        for action in app.tree.handle_event(UiEvent::Text("// otra cosa\n".into())) {
            app.apply_action(action, &egui::Context::default());
        }
        let payload = prepare_native_attachments_with_image_support(
            Path::new(&app.view.workspace),
            &app.view.composer_attachments,
            false,
        )
        .unwrap();
        let prompt = native_prompt_with_images(
            "Explica esto",
            &payload.path_attachments,
            &payload.image_attachments,
        );
        assert_eq!(
            prompt,
            "Explica esto\n\nAdjuntos de este proyecto (rutas relativas): \
             [{\"lines\":\"2-3\",\"path\":\"src/main.rs\",\"type\":\"selection\"}]\n\
             Abre los archivos y explora las carpetas pertinentes; úsalos como contexto para esta tarea.\
             \n\nSelección de src/main.rs (líneas 2-3), tal como estaba al adjuntarla:\n\
             ```\n    let saludo = \"hola\";\n    println!(\"{saludo}\");\n```"
        );
        // A message with a selection is recognised as carrying attachments.
        assert!(crate::message_edit::carries_attachments(&prompt, 0));

        // Text that holds a code fence is wrapped in a longer one.
        let fenced = ComposerAttachmentView {
            path: "README.md".into(),
            label: "README.md".into(),
            is_dir: false,
            selection: Some(AttachedSelection {
                start_line: 7,
                end_line: 7,
                text: "```rust".into(),
            }),
        };
        assert!(composer_prompt_with_attachments("", &[fenced.clone()])
            .ends_with("(líneas 7), tal como estaba al adjuntarla:\n````\n```rust\n````"));

        // The draft of a message sent to a running turn keeps the selection.
        let draft = NativeSteerDraftState::for_draft(Path::new("/p"), "s", "", &[fenced.clone()]);
        let stored: NativeSteerDraftState =
            serde_json::from_slice(&serde_json::to_vec(&draft).unwrap()).unwrap();
        assert_eq!(ComposerAttachmentView::from(stored.attachments[0].clone()), fenced);
    }

    #[test]
    fn selections_of_one_message_stay_under_their_size_limit() {
        let (_root, mut app) = fixture();
        app.attach_selection(
            "big.txt".into(),
            AttachedSelection { start_line: 1, end_line: 1, text: "a".repeat(MAX_SELECTION_BYTES) },
        );
        assert_eq!(app.view.composer_attachments.len(), 1);
        select(&mut app, 0, 5);
        app.add_editor_selection_to_chat();
        assert_eq!(app.view.composer_attachments.len(), 1);
        assert!(app.view.status.contains("16 KiB"));
    }

    /// A CLI task whose terminal runs `cat`: it shows what was typed into it.
    #[cfg(unix)]
    #[test]
    fn a_cli_task_gets_the_reference_typed_into_its_terminal_and_no_chip() {
        let (root, mut app) = fixture();
        let (agent, _) = crate::agent_cli_definitions::profile("claude").unwrap();
        select(&mut app, LINES_2_3.0, LINES_2_3.1);
        app.view.selected_session = Some("cli-task".into());
        app.view.selected_backend = format!("{}claude", crate::view::CLI_BACKEND);
        // The editor belongs to the scope of the task now selected.
        let editor = app.view.code_editor.clone();
        app.add_editor_selection_to_chat();
        assert!(app.view.status.contains("no está en ejecución"), "{}", app.view.status);

        let id = app.new_agent_terminal((agent, false));
        let session = crate::terminal::TerminalSession::spawn_with_command(
            root.path(),
            60,
            6,
            std::ffi::OsStr::new("cat"),
            &[],
        )
        .unwrap();
        app.terminals.insert(id, session);
        app.view.code_editor = editor;
        app.add_editor_selection_to_chat();

        assert!(app.view.composer_attachments.is_empty());
        assert_eq!(app.view.status, "Selección enviada a Claude Code: @src/main.rs#L2-3");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let session = app.terminals.get_mut(&id).unwrap();
            session.poll();
            let lines = session.snapshot().lines;
            if lines.iter().any(|line| line.contains("@src/main.rs#L2-3")) {
                // Typed, not submitted: `cat` would have repeated a sent line.
                assert_eq!(lines.iter().filter(|line| line.contains("@src")).count(), 1);
                break;
            }
            assert!(Instant::now() < deadline, "the reference never reached the terminal");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "writes PNG previews under docs/ui-separation/shots/selection-chat"]
    fn export_selection_chat_visual_review() {
        use crate::view::ide_visual_tests::{export_shot, native_tree};
        let (_root, mut app) = fixture();
        let focused = app.tree.focused_id().unwrap().to_owned();
        let (context, tree) = native_tree(Element::spacer());
        app.tree = tree;
        frame(&mut app);
        assert!(app.tree.focus(&focused));
        select(&mut app, LINES_2_3.0, LINES_2_3.1);
        assert!(rested(&mut app).is_some());
        export_shot(&frame(&mut app), VIEWPORT, "selection-chat", "s1-control-over-selection");
        click(&mut app, selection_chat::ACTION);
        export_shot(&frame(&mut app), VIEWPORT, "selection-chat", "s2-chip-in-composer");
        context.end_pass().textures_delta.clear();
    }
}
