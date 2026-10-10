//! Open an exact chat row as a disk document or immutable recorded evidence.
use super::*;
use crate::{
    code_snapshot_ui::{self, CodeSnapshotContent, CodeSnapshotView},
    file_activity_preview,
    tool_presentation::{self, ToolNavigationTarget},
    transcript::{ActivityStatus, ModelStreamStatus},
};
use openide_editor::TextBuffer;
use openide_ui::{CodeLanguage, EditorController};
use std::path::Component as PathComponent;

const MAX_SNAPSHOTS: usize = 16;

/// Reject escapes even when the destination is missing. A symlinked existing
/// ancestor must resolve inside the same workspace before a snapshot is offered.
fn workspace_destination(workspace: &Path, raw: &str) -> Option<(PathBuf, bool)> {
    if raw.is_empty() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return None;
    }
    let path = Path::new(raw);
    if path
        .components()
        .any(|part| matches!(part, PathComponent::ParentDir))
        || (!path.is_absolute()
            && (path.has_root()
                || path
                    .components()
                    .any(|part| matches!(part, PathComponent::Prefix(_)))))
    {
        return None;
    }
    let root = workspace.canonicalize().ok()?;
    if !root.is_dir() {
        return None;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if let Ok(actual) = absolute.canonicalize() {
        let relative = actual.strip_prefix(&root).ok()?.to_path_buf();
        return actual.is_file().then_some((relative, true));
    }
    let mut ancestor = absolute.as_path();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                // Resolve the ancestor before checking containment: an absolute
                // alias of the workspace need not share the root's lexical prefix.
                // symlink_metadata sees dangling links, so canonicalize fails
                // here instead of silently skipping them as missing parents.
                let resolved = ancestor.canonicalize().ok()?;
                if !resolved.is_dir() || !resolved.starts_with(&root) {
                    return None;
                }
                let suffix = absolute.strip_prefix(ancestor).ok()?;
                if !suffix
                    .components()
                    .any(|part| matches!(part, PathComponent::Normal(_)))
                {
                    return None;
                }
                let destination = resolved.join(suffix);
                let relative = destination.strip_prefix(&root).ok()?.to_path_buf();
                return Some((relative, false));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor.parent()?
            }
            Err(_) => return None,
        }
    }
}

fn row_identity(item: &TranscriptItem) -> Option<(&str, &str)> {
    match item {
        TranscriptItem::Tool { id, name, .. }
        | TranscriptItem::ToolPreview {
            id,
            name: Some(name),
            ..
        } if file_activity_preview::is_file_tool(name) => Some((id, name)),
        _ => None,
    }
}

fn snapshot_caption(item: &TranscriptItem, name: &str, limited: bool, deleted: bool) -> String {
    let state = match item {
        TranscriptItem::Tool {
            activity: ActivityStatus::Discarded,
            ..
        }
        | TranscriptItem::ToolPreview {
            status: ModelStreamStatus::Discarded,
            ..
        } => "Propuesta descartada · sin ejecutar",
        TranscriptItem::Tool { is_error: true, .. }
        | TranscriptItem::Tool {
            activity:
                ActivityStatus::Failed
                | ActivityStatus::Cancelled
                | ActivityStatus::Interrupted
                | ActivityStatus::PermissionInterrupted,
            ..
        }
        | TranscriptItem::ToolPreview {
            status:
                ModelStreamStatus::Cancelled
                | ModelStreamStatus::Interrupted
                | ModelStreamStatus::Failed,
            ..
        } => "Contenido enviado · sin confirmación",
        _ if name == "read_file" => "Lectura capturada",
        TranscriptItem::Tool {
            activity: ActivityStatus::Completed,
            ..
        } => "Contenido enviado registrado",
        _ => "Propuesta capturada",
    };
    format!(
        "{state}{}{} · solo lectura · no es el archivo en disco",
        if limited { " · vista parcial" } else { "" },
        if deleted {
            " · se propone eliminar este archivo"
        } else {
            ""
        }
    )
}

impl AgentsWindow {
    pub(super) fn sync_code_snapshots(&mut self) {
        let retired = self
            .code_snapshots
            .keys()
            .copied()
            .filter(|id| !self.dock.contains_tab(*id))
            .collect::<Vec<_>>();
        for id in retired {
            self.code_snapshots.remove(&id);
            self.tree.forget_code_view(&code_snapshot_ui::view_id(id));
        }
        self.view.code_snapshot = self
            .dock
            .active_tab(&self.dock_scope())
            .and_then(|tab| {
                self.code_snapshots.get(&tab.id.get()).filter(|snapshot| {
                    matches!(&tab.kind, DockTabKind::CodeSnapshot { source, title }
                        if source == &snapshot.source && title == &snapshot.title)
                })
            })
            .cloned();
    }

    pub(super) fn apply_file_activity_open(&mut self, action: &UiAction) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if let Some(raw) = id.strip_prefix("file_activity_directory:") {
            if raw.len() > 16 * 1024 {
                return true;
            }
            let Ok((owner, row)) = serde_json::from_str::<(String, String)>(raw) else {
                return true;
            };
            if self.view.selected_session.as_deref() != Some(owner.as_str()) {
                return true;
            }
            let mut matching = self.view.transcript.iter().filter(|item| {
                matches!(item, TranscriptItem::Tool { id, .. } | TranscriptItem::ToolPreview { id, .. } if id == &row)
            });
            let Some(TranscriptItem::Tool {
                name, arguments, ..
            }) = matching.next()
            else {
                return true;
            };
            if name != "list_files" || matching.next().is_some() {
                return true;
            }
            let raw_path = arguments
                .get("path")
                .and_then(|path| path.as_str())
                .unwrap_or(".");
            if raw_path.len() > 4096
                || raw_path.chars().any(char::is_control)
                || Path::new(raw_path)
                    .components()
                    .any(|part| matches!(part, PathComponent::ParentDir))
            {
                return true;
            }
            let relative = Path::new(&self.view.workspace)
                .canonicalize()
                .ok()
                .and_then(|root| {
                    let directory = root.join(raw_path).canonicalize().ok()?;
                    directory.is_dir().then_some(())?;
                    directory.strip_prefix(root).ok().map(Path::to_path_buf)
                });
            if let Some(relative) = relative {
                match self.reveal_editor_folder(&relative) {
                    Ok(()) => {
                        self.close_environment();
                        self.view.show_dock_overview = false;
                    }
                    Err(error) => self.view.status = error,
                }
            } else {
                self.view.status = "La carpeta no está disponible dentro del proyecto".into();
            }
            return true;
        }
        if let Some(raw) = id.strip_prefix("file_activity_inspect:") {
            if raw.len() > 16 * 1024 {
                return true;
            }
            let Ok((owner, row)) = serde_json::from_str::<(String, String)>(raw) else {
                return true;
            };
            if self.view.selected_session.as_deref() != Some(owner.as_str()) {
                return true;
            }
            let mut matching = self.view.transcript.iter().filter(|item| {
                matches!(item, TranscriptItem::Tool { id, .. } | TranscriptItem::ToolPreview { id, .. } if id == &row)
            });
            if !matches!(matching.next(), Some(TranscriptItem::Tool { name, .. })
                if file_activity_preview::is_file_tool(name) || crate::search_activity_ui::supported(name))
                || matching.next().is_some()
            {
                return true;
            }
            self.view.selected_tool_id = Some(row);
            self.view.show_side_pane = true;
            self.view.side_tab = SideTab::Activity;
            let scope = self.dock_scope();
            self.dock.open(&scope, DockTabKind::Activity);
            self.view.show_dock_overview = false;
            self.sync_dock_view();
            return true;
        }
        let Some(raw) = id.strip_prefix("file_activity_open:") else {
            return false;
        };
        if raw.len() > 16 * 1024 {
            return true;
        }
        let Ok((owner, row, index)) = serde_json::from_str::<(String, String, usize)>(raw) else {
            return true;
        };
        if self.view.selected_session.as_deref() != Some(owner.as_str()) || index >= 8 {
            return true;
        }
        let mut matches = self.view.transcript.iter().filter(|item| {
            matches!(item, TranscriptItem::Tool { id, .. } | TranscriptItem::ToolPreview { id, .. } if id == &row)
        });
        let Some(item) = matches.next() else {
            return true;
        };
        if matches.next().is_some() {
            return true;
        }
        let Some((_, name)) = row_identity(item) else {
            return true;
        };
        let preview = file_activity_preview::preview(item, None);
        let file = preview
            .as_ref()
            .and_then(|preview| preview.files.get(index));
        let fallback = if index == 0 {
            match item {
                TranscriptItem::Tool { arguments, .. } => {
                    match tool_presentation::navigation_target(name, arguments) {
                        Some(ToolNavigationTarget::File(path)) => Some(path.to_owned()),
                        _ => None,
                    }
                }
                TranscriptItem::ToolPreview { arguments, .. } => {
                    file_activity_preview::preview_path(arguments)
                }
                _ => None,
            }
        } else {
            None
        };
        let Some(path) = file.map(|file| file.path.as_str()).or(fallback.as_deref()) else {
            return true;
        };
        let Some((relative, exists)) = workspace_destination(Path::new(&self.view.workspace), path)
        else {
            self.view.status = "El archivo no está disponible dentro del proyecto".into();
            return true;
        };
        if exists {
            self.close_environment();
            if self
                .dock
                .open_file(Path::new(&self.view.workspace), &owner, &relative)
                .is_some()
            {
                self.view.show_dock_overview = false;
                self.sync_dock_view();
                self.refresh_clean_code_editor();
            }
            return true;
        }
        let Some(file) = file else {
            self.view.status =
                "El archivo todavía no existe y no hay contenido registrado para mostrar".into();
            return true;
        };
        let source = serde_json::to_string(&(owner.as_str(), row.as_str(), index))
            .expect("scope serializes");
        let scope = self.dock_scope();
        // A repeated click reuses the original capture instead of replacing its
        // content with later stream tokens while the user is reading it.
        if let Some(id) = self.dock.visible_tabs(&scope).into_iter().find_map(|tab| {
            matches!(&tab.kind, DockTabKind::CodeSnapshot { source: previous, .. } if previous == &source)
                .then_some(tab.id)
        }) {
            self.close_environment();
            self.dock.activate(&scope, id);
            self.dock.set_collapsed(&scope, false);
            self.view.show_dock_overview = false;
            self.sync_dock_view();
            return true;
        }
        let limited = preview.as_ref().is_some_and(|preview| preview.limited);
        let language = CodeLanguage::from_extension(
            Path::new(path)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default(),
        );
        let exact_write = match item {
            TranscriptItem::Tool { arguments, .. } if name == "write_file" && !limited => arguments
                .get("content")
                .and_then(serde_json::Value::as_str)
                .filter(|text| text.len() <= 32 * 1024),
            _ => None,
        };
        let content = exact_write
            .and_then(|text| TextBuffer::new(text).ok())
            .map(|buffer| CodeSnapshotContent::Text {
                controller: EditorController::new(buffer),
                language,
            })
            .unwrap_or_else(|| CodeSnapshotContent::Diff {
                lines: file.lines.clone(),
                language,
                single_gutter: matches!(name, "read_file" | "write_file"),
            });
        let path = path.to_owned();
        let title = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&path)
            .to_owned();
        let caption = snapshot_caption(item, name, limited, file.deleted);
        self.sync_code_snapshots();
        if self.code_snapshots.len() >= MAX_SNAPSHOTS {
            self.view.status = "Cierra una captura antes de abrir otra (máximo 16)".into();
            return true;
        }
        self.close_environment();
        let id = self.dock.open(
            &scope,
            DockTabKind::CodeSnapshot {
                source: source.clone(),
                title: title.clone(),
            },
        );
        self.code_snapshots.insert(
            id.get(),
            CodeSnapshotView {
                review: None,
                tab_id: id.get(),
                source,
                title,
                path,
                caption,
                content,
            },
        );
        self.view.show_dock_overview = false;
        self.sync_dock_view();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.workspace_draft = app.view.workspace.clone();
        app.view.selected_session = Some("owner".into());
        app.view.show_side_pane = false;
        app.view.transcript.clear();
        (root, app)
    }

    fn write(id: &str, path: &str, content: &str) -> TranscriptItem {
        TranscriptItem::Tool {
            id: id.into(),
            name: "write_file".into(),
            arguments: json!({"path":path,"content":content}),
            output: None,
            is_error: false,
            activity: ActivityStatus::Running,
        }
    }

    fn click(app: &mut AgentsWindow, owner: &str, row: &str, index: usize) {
        assert!(app.apply_file_activity_open(&UiAction::Clicked {
            id: format!(
                "file_activity_open:{}",
                serde_json::to_string(&(owner, row, index)).unwrap()
            ),
        }));
    }

    #[test]
    fn file_activity_inspection_opens_the_scoped_tool_without_opening_or_expanding_the_file() {
        let (_root, mut app) = fixture();
        app.view
            .transcript
            .push(write("call:one", "src/new.rs", "proposed"));
        app.view.conversation_details.sync(Some("owner"));
        app.view
            .conversation_details
            .set_expanded("tool:call:one", false);
        assert!(app.apply_file_activity_open(&UiAction::Clicked {
            id: crate::file_activity_ui::inspect_action("owner", "call:one"),
        }));
        assert_eq!(app.view.selected_tool_id.as_deref(), Some("call:one"));
        assert_eq!(app.view.side_tab, SideTab::Activity);
        assert!(app.view.show_side_pane);
        assert!(!app
            .view
            .conversation_details
            .is_expanded("tool:call:one", true));
        assert!(app.code_snapshots.is_empty());
        assert!(!Path::new(&app.view.workspace).join("src/new.rs").exists());
    }

    #[test]
    fn file_activity_inspection_rejects_stale_ambiguous_and_non_tool_rows() {
        let (_root, mut app) = fixture();
        let mut other_tool = write("call", "src/new.rs", "proposed");
        if let TranscriptItem::Tool { name, .. } = &mut other_tool {
            *name = "run_command".into();
        }
        let preview = TranscriptItem::ToolPreview {
            id: "call".into(),
            step: 1,
            item_id: "item".into(),
            call_id: Some("future".into()),
            name: Some("write_file".into()),
            arguments: "{}".into(),
            input_complete: false,
            status: ModelStreamStatus::Receiving,
            truncated: false,
        };
        for (owner, row, items) in [
            ("other", "call", vec![write("call", "src/a.rs", "a")]),
            ("owner", "missing", vec![write("call", "src/a.rs", "a")]),
            (
                "owner",
                "call",
                vec![
                    write("call", "src/a.rs", "a"),
                    write("call", "src/b.rs", "b"),
                ],
            ),
            ("owner", "call", vec![preview.clone()]),
            (
                "owner",
                "call",
                vec![write("call", "src/a.rs", "a"), preview],
            ),
            ("owner", "call", vec![other_tool]),
        ] {
            app.view.transcript = items;
            assert!(app.apply_file_activity_open(&UiAction::Clicked {
                id: crate::file_activity_ui::inspect_action(owner, row),
            }));
            assert_eq!(app.view.selected_tool_id, None);
            assert!(!app.view.show_side_pane);
            assert!(app.code_snapshots.is_empty());
        }
    }

    #[test]
    fn directory_and_search_titles_open_the_current_scoped_activity() {
        let (_root, mut app) = fixture();
        for name in [
            "list_files",
            "find_files",
            "search_files",
            "search_text",
            "web_search",
            "web_fetch",
        ] {
            app.view.transcript = vec![TranscriptItem::Tool {
                id: name.into(),
                name: name.into(),
                arguments: json!({"path":"."}),
                output: None,
                is_error: false,
                activity: ActivityStatus::Running,
            }];
            assert!(app.apply_file_activity_open(&UiAction::Clicked {
                id: crate::file_activity_ui::inspect_action("owner", name),
            }));
            assert_eq!(app.view.selected_tool_id.as_deref(), Some(name));
            assert_eq!(app.view.side_tab, SideTab::Activity);
            assert!(app.code_snapshots.is_empty());
            assert!(app.apply_file_activity_open(&UiAction::Clicked {
                id: crate::file_activity_ui::inspect_action("stale", "missing"),
            }));
            assert_eq!(app.view.selected_tool_id.as_deref(), Some(name));
        }
    }

    #[test]
    fn directory_titles_reveal_the_captured_folder_without_toggling_the_transcript() {
        let (_root, mut app) = fixture();
        let workspace = PathBuf::from(&app.view.workspace);
        std::fs::create_dir_all(workspace.join("apps/nested")).unwrap();
        app.view.conversation_details.sync(Some("owner"));
        app.view
            .conversation_details
            .set_expanded("tool:listing", false);
        for path in [".", "apps", "apps/nested"] {
            app.view.transcript = vec![TranscriptItem::Tool {
                id: "listing".into(),
                name: "list_files".into(),
                arguments: json!({"path":path}),
                output: None,
                is_error: false,
                activity: ActivityStatus::Running,
            }];
            assert!(app.apply_file_activity_open(&UiAction::Clicked {
                id: crate::file_activity_ui::directory_action("owner", "listing"),
            }));
            assert_eq!(app.view.side_tab, SideTab::Files);
            assert_eq!(app.view.selected_tool_id, None);
            assert!(!app
                .view
                .conversation_details
                .is_expanded("tool:listing", true));
            assert!(app.code_snapshots.is_empty());
        }
        for (name, depth) in [("apps", 0), ("nested", 1)] {
            assert!(
                app.view.file_rows.iter().any(|row| row.name == name
                    && row.depth == depth
                    && row.is_dir
                    && row.expanded),
                "revealed ancestor {name} must remain visible and expanded"
            );
        }
    }

    #[test]
    fn directory_navigation_rejects_stale_rows_files_and_workspace_escapes() {
        let (_root, mut app) = fixture();
        let workspace = PathBuf::from(&app.view.workspace);
        std::fs::write(workspace.join("file.txt"), "content").unwrap();
        for (owner, path, repeated) in [
            ("stale", ".", false),
            ("owner", "../", false),
            ("owner", "/", false),
            ("owner", "missing", false),
            ("owner", "file.txt", false),
            ("owner", ".", true),
        ] {
            let item = TranscriptItem::Tool {
                id: "listing".into(),
                name: "list_files".into(),
                arguments: json!({"path":path}),
                output: None,
                is_error: false,
                activity: ActivityStatus::Completed,
            };
            app.view.transcript = vec![item.clone()];
            if repeated {
                app.view.transcript.push(item);
            }
            assert!(app.apply_file_activity_open(&UiAction::Clicked {
                id: crate::file_activity_ui::directory_action(owner, "listing"),
            }));
            assert!(!app.view.show_side_pane);
            assert_eq!(app.view.selected_tool_id, None);
            assert!(app.code_snapshots.is_empty());
        }
    }

    #[test]
    fn missing_write_opens_exact_read_only_capture_and_leaves_disk_untouched() {
        let (_root, mut app) = fixture();
        app.view
            .transcript
            .push(write("call", "src/new.rs", "let café = 1;\r\n"));
        click(&mut app, "owner", "call", 0);
        assert_eq!(app.view.side_tab, SideTab::CodeSnapshot);
        assert!(app.view.show_side_pane);
        assert!(!Path::new(&app.view.workspace).join("src/new.rs").exists());
        let view = app.view.code_snapshot.as_ref().unwrap();
        assert!(view.caption.contains("Propuesta capturada"));
        assert!(view.caption.contains("no es el archivo en disco"));
        let CodeSnapshotContent::Text { controller, .. } = &view.content else {
            panic!("complete write capture");
        };
        let snapshot = controller.snapshot().unwrap();
        assert_eq!(
            snapshot.text_range(0..snapshot.char_count()).unwrap(),
            "let café = 1;\r\n"
        );
        assert!(!snapshot.is_dirty());
        let actions = UiAction::EditorSaveRequested {
            id: code_snapshot_ui::view_id(view.tab_id),
            document_id: snapshot.document_id(),
            revision: snapshot.revision(),
        };
        app.apply_code_editor_action(&actions, &egui::Context::default());
        assert!(!Path::new(&app.view.workspace).join("src/new.rs").exists());
    }

    #[test]
    fn owner_duplicates_and_invalid_file_index_never_choose_a_different_row() {
        let (_root, mut app) = fixture();
        app.view.transcript.push(write("call", "new.rs", "first"));
        for (owner, row, index) in [
            ("other", "call", 0),
            ("owner", "absent", 0),
            ("owner", "call", 1),
        ] {
            click(&mut app, owner, row, index);
            assert!(app.code_snapshots.is_empty());
        }
        app.view
            .transcript
            .push(write("call", "other.rs", "second"));
        click(&mut app, "owner", "call", 0);
        assert!(app.code_snapshots.is_empty());
        assert!(!app.view.show_side_pane);
    }

    #[test]
    fn path_escape_is_rejected_for_existing_and_missing_targets() {
        let (root, mut app) = fixture();
        let outside = root.path().join("outside.rs");
        std::fs::write(&outside, "outside").unwrap();
        for path in [
            "../outside.rs".to_owned(),
            "../missing.rs".to_owned(),
            outside.to_string_lossy().into_owned(),
        ] {
            app.view.transcript = vec![write("call", &path, "proposed")];
            click(&mut app, "owner", "call", 0);
            assert!(!app.view.show_side_pane, "{path}");
            assert!(app.code_snapshots.is_empty());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path(), Path::new(&app.view.workspace).join("escape"))
                .unwrap();
            app.view.transcript = vec![write("call", "escape/missing.rs", "proposed")];
            click(&mut app, "owner", "call", 0);
            assert!(!app.view.show_side_pane);
        }
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside");
    }

    #[cfg(unix)]
    #[test]
    fn missing_absolute_alias_resolves_against_workspace_without_accepting_broken_links() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let alias = temp.path().join("project-alias");
        std::os::unix::fs::symlink(&workspace, &alias).unwrap();
        assert_eq!(
            workspace_destination(&workspace, alias.join("new/child.rs").to_str().unwrap()),
            Some((PathBuf::from("new/child.rs"), false))
        );
        assert_eq!(
            workspace_destination(&alias, alias.join("new/child.rs").to_str().unwrap()),
            Some((PathBuf::from("new/child.rs"), false))
        );
        std::os::unix::fs::symlink(workspace.join("absent"), workspace.join("broken")).unwrap();
        for path in ["broken", "broken/child.rs"] {
            assert!(workspace_destination(&workspace, path).is_none(), "{path}");
        }
        std::os::unix::fs::symlink(temp.path(), workspace.join("outside")).unwrap();
        assert!(workspace_destination(&workspace, "outside/new/child.rs").is_none());
        assert!(!workspace.join("new").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_absolute_paths_accept_their_prefix_but_drive_relative_paths_do_not() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let existing = workspace.join("existing.rs");
        std::fs::write(&existing, "disk").unwrap();
        assert_eq!(
            workspace_destination(&workspace, existing.to_str().unwrap()),
            Some((PathBuf::from("existing.rs"), true))
        );
        assert_eq!(
            workspace_destination(&workspace, workspace.join("new/child.rs").to_str().unwrap()),
            Some((PathBuf::from("new/child.rs"), false))
        );
        for raw in ["C:relative.rs", "\\relative.rs"] {
            assert!(workspace_destination(&workspace, raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn existing_confirmed_file_uses_editable_disk_document_not_captured_output() {
        let (_root, mut app) = fixture();
        std::fs::write(
            Path::new(&app.view.workspace).join("src/real.rs"),
            "current disk\n",
        )
        .unwrap();
        app.view.transcript.push(TranscriptItem::Tool {
            id: "read".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/real.rs"}),
            output: Some("earlier capture".into()),
            is_error: false,
            activity: ActivityStatus::Completed,
        });
        click(&mut app, "owner", "read", 0);
        assert_eq!(app.view.side_tab, SideTab::FilePreview);
        assert!(app.view.show_side_pane);
        assert!(app.code_snapshots.is_empty());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll_code_editor(&egui::Context::default());
            if app
                .view
                .code_editor
                .as_ref()
                .is_some_and(|view| !view.loading)
            {
                break;
            }
            assert!(Instant::now() < deadline, "editor load timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        let view = app.view.code_editor.as_ref().unwrap();
        let snapshot = view.controller.as_ref().unwrap().snapshot().unwrap();
        assert_eq!(
            snapshot.text_range(0..snapshot.char_count()).unwrap(),
            "current disk\n"
        );
        assert!(app.view.code_snapshot.is_none());
    }

    #[test]
    fn read_capture_retains_original_line_numbers_and_error_output_is_not_source() {
        let (_root, mut app) = fixture();
        app.view.transcript.push(TranscriptItem::Tool {
            id: "read".into(),
            name: "read_file".into(),
            arguments: json!({"path":"gone.rs","start_line":45}),
            output: Some("alpha\nbeta\n".into()),
            is_error: false,
            activity: ActivityStatus::Completed,
        });
        click(&mut app, "owner", "read", 0);
        let view = app.view.code_snapshot.as_ref().unwrap();
        let CodeSnapshotContent::Diff {
            lines,
            single_gutter,
            ..
        } = &view.content
        else {
            panic!("captured range");
        };
        assert!(*single_gutter);
        assert_eq!(lines[0].new_line, Some(45));
        assert_eq!(lines[1].new_line, Some(46));
        app.view.transcript.push(TranscriptItem::Tool {
            id: "error".into(),
            name: "read_file".into(),
            arguments: json!({"path":"missing.rs"}),
            output: Some("permission denied".into()),
            is_error: true,
            activity: ActivityStatus::Failed,
        });
        click(&mut app, "owner", "error", 0);
        assert_eq!(app.code_snapshots.len(), 1);
    }

    #[test]
    fn streaming_capture_is_immutable_and_closed_views_are_released() {
        let (_root, mut app) = fixture();
        let preview = |text: &str| TranscriptItem::ToolPreview {
            id: "preview:1".into(),
            step: 1,
            item_id: "item".into(),
            call_id: Some("call".into()),
            name: Some("write_file".into()),
            arguments: format!("{{\"path\":\"missing.rs\",\"content\":\"{text}"),
            input_complete: false,
            status: ModelStreamStatus::Receiving,
            truncated: false,
        };
        app.view.transcript.push(preview("first"));
        click(&mut app, "owner", "preview:1", 0);
        let before = app.view.code_snapshot.clone().unwrap();
        let CodeSnapshotContent::Diff { lines, .. } = &before.content else {
            panic!("partial input");
        };
        assert_eq!(lines[0].text, "first");
        app.view.transcript[0] = preview("first later");
        click(&mut app, "owner", "preview:1", 0);
        let CodeSnapshotContent::Diff { lines: after, .. } =
            &app.view.code_snapshot.as_ref().unwrap().content
        else {
            panic!("capture");
        };
        assert!(std::sync::Arc::ptr_eq(lines, after));
        assert_eq!(app.code_snapshots.len(), 1);
        let scope = app.dock_scope();
        app.dock
            .close(&scope, DockTabId::from_raw(before.tab_id).unwrap());
        app.sync_dock_view();
        assert!(app.code_snapshots.is_empty());
        assert!(app.view.code_snapshot.is_none());
    }

    #[test]
    fn multi_file_patch_uses_requested_file_and_snapshot_budget_stays_bounded() {
        let (_root, mut app) = fixture();
        app.view.transcript.push(TranscriptItem::Tool {
            id: "patch".into(), name: "apply_patch".into(),
            arguments: json!({"patch":"*** Begin Patch\n*** Add File: one.rs\n+one\n*** Add File: two.rs\n+two\n*** End Patch\n"}),
            output: None, is_error: false, activity: ActivityStatus::Running,
        });
        click(&mut app, "owner", "patch", 1);
        let view = app.view.code_snapshot.as_ref().unwrap();
        assert_eq!(view.path, "two.rs");
        let CodeSnapshotContent::Diff {
            lines,
            single_gutter,
            ..
        } = &view.content
        else {
            panic!("patch diff");
        };
        assert!(!single_gutter);
        assert_eq!(lines[0].text, "two");
        assert_eq!(lines[0].kind, openide_ui::CodeDiffLineKind::Added);
        for index in 0..20 {
            let id = format!("call:{index}");
            app.view
                .transcript
                .push(write(&id, &format!("missing-{index}.rs"), "bounded"));
            click(&mut app, "owner", &id, 0);
        }
        assert_eq!(app.code_snapshots.len(), MAX_SNAPSHOTS);
        assert!(app.view.status.contains("máximo 16"));
    }
}
