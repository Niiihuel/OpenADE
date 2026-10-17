//! Explicit navigation from confirmed tool arguments, scoped to the active chat.

use super::*;
use crate::tool_presentation::{self, ToolNavigationTarget};

impl AgentsWindow {
    /// Raw provider call IDs can be reused in another response. An action that
    /// names only that ID cannot choose a historical occurrence safely.
    pub(super) fn tool_navigation_is_unique(&self, owner: &str, call_id: &str) -> bool {
        self.view.selected_session.as_deref() == Some(owner)
            && self
                .view
                .transcript
                .iter()
                .filter(|item| matches!(item, TranscriptItem::Tool { id, .. } if id == call_id))
                .take(2)
                .count()
                == 1
    }

    pub(super) fn apply_tool_navigation_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if let Some(scope) = id.strip_prefix("tool_image:") {
            self.open_tool_image(scope, ctx);
            return true;
        }
        if let Some(scope) = id.strip_prefix("chat_diff_file:") {
            let Ok((owner, call_id, file_index)) =
                serde_json::from_str::<(String, String, usize)>(scope)
            else {
                return true;
            };
            if !self.tool_navigation_is_unique(&owner, &call_id) {
                return true;
            }
            let path = crate::chat_diff_ui::entry(&self.view, &call_id)
                .and_then(|entry| entry.files.get(file_index))
                .filter(|file| file.can_open)
                .map(|file| file.path.clone());
            if let Some(path) = path {
                self.open_tool_workspace_file(&owner, &path);
            }
            return true;
        }
        let Some(scope) = id.strip_prefix("tool_navigate:") else {
            return false;
        };
        let Some((owner, call_id)) = serde_json::from_str::<(String, String)>(scope).ok() else {
            return true;
        };
        if !self.tool_navigation_is_unique(&owner, &call_id) {
            return true;
        }
        let target = self.view.transcript.iter().find_map(|item| match item {
            TranscriptItem::Tool {
                id,
                name,
                arguments,
                ..
            } if id == &call_id => tool_presentation::navigation_target(name, arguments),
            _ => None,
        });
        match target {
            Some(ToolNavigationTarget::File(path)) => {
                let path = path.to_owned();
                self.open_tool_workspace_file(&owner, &path);
            }
            Some(ToolNavigationTarget::Url(raw)) => {
                // Re-resolve from the confirmed row at click time. The action
                // contains no URL and cannot redirect a stale session's button.
                let Ok(url) = reqwest::Url::parse(raw) else {
                    return true;
                };
                ctx.open_url(egui::OpenUrl::new_tab(url.to_string()));
            }
            None => {}
        }
        true
    }

    pub(super) fn open_tool_workspace_file(&mut self, owner: &str, path: &str) {
        let workspace = Path::new(&self.view.workspace);
        let relative = workspace.canonicalize().ok().and_then(|root| {
            let path = workspace.join(path).canonicalize().ok()?;
            path.is_file().then_some(())?;
            path.strip_prefix(root).ok().map(Path::to_path_buf)
        });
        if let Some(relative) = relative {
            self.close_environment();
            if self
                .dock
                .open_file(Path::new(&self.view.workspace), owner, &relative)
                .is_some()
            {
                self.sync_dock_view();
                self.refresh_clean_code_editor();
            }
        } else {
            self.view.status = "El archivo ya no está disponible dentro del proyecto".into();
        }
    }

    fn open_tool_image(&mut self, scope: &str, ctx: &egui::Context) {
        let Ok((owner, call_id, index, digest)) =
            serde_json::from_str::<(String, String, usize, String)>(scope)
        else {
            return;
        };
        if !self.tool_navigation_is_unique(&owner, &call_id) {
            return;
        }
        let Some(tool_index) = self
            .view
            .transcript
            .iter()
            .position(|item| matches!(item, TranscriptItem::Tool { id, .. } if id == &call_id))
        else {
            return;
        };
        let Some(TranscriptItem::ToolMedia { images, .. }) =
            crate::session_surface::tool_media(&self.view, tool_index, &call_id)
        else {
            return;
        };
        let Some(image) = images.get(index).and_then(|image| image.raster.as_ref()) else {
            return;
        };
        if image.sha256() != digest {
            return;
        }
        let image = image.clone();
        let selected = images[..index]
            .iter()
            .filter(|image| image.raster.is_some())
            .count();
        let images = images
            .iter()
            .filter_map(|image| image.raster.clone())
            .collect();
        self.close_composer_floating_controls();
        // Invalidate any pending attachment-worker response. The payload is
        // already in the journal: opening the viewer performs no file/network IO.
        self.attachment_viewer_generation = self.attachment_viewer_generation.wrapping_add(1);
        self.view.composer_image_viewer = Some(crate::composer_attachments::ImageGalleryView {
            workspace: self.view.workspace.clone(),
            attachments: Vec::new(),
            selected,
            generation: self.attachment_viewer_generation,
            image: Some(image),
            error: None,
            loading: false,
            zoom: 1.0,
            tool_source: Some(crate::composer_attachments::ToolImageGallery {
                owner,
                call_id,
                images,
            }),
        });
        self.view.composer_add_menu_open = false;
        self.view.composer_model_menu_open = false;
        self.view.composer_project_menu_open = false;
        self.view.backend_menu_open = false;
        self.prompt_focus_pending = false;
        self.dock_drag = None;
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{ActivityStatus, ModelStreamStatus};
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.workspace = workspace.to_string_lossy().into_owned();
        app.view.selected_session = Some("owner".into());
        app.view.show_side_pane = false;
        app.view.transcript.push(TranscriptItem::Tool {
            id: "call".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/main.rs", "start_line":3}),
            output: Some("fn main() {}".into()),
            is_error: false,
            activity: ActivityStatus::Completed,
        });
        (temp, app)
    }
    fn click(app: &mut AgentsWindow, owner: &str, call: &str) {
        assert!(app.apply_tool_navigation_action(
            &UiAction::Clicked {
                id: tool_presentation::navigation_action(owner, call),
            },
            &egui::Context::default()
        ));
    }

    #[test]
    fn exact_scoped_file_action_opens_the_native_editor_from_original_arguments() {
        let (_temp, mut app) = fixture();
        click(&mut app, "old-session", "call");
        assert!(!app.view.show_side_pane);
        click(&mut app, "owner", "missing-call");
        assert!(!app.view.show_side_pane);
        click(&mut app, "owner", "call");
        assert!(app.view.show_side_pane);
        assert_eq!(
            app.view
                .file_preview
                .as_ref()
                .map(|view| view.path.as_str()),
            Some("src/main.rs")
        );
        assert_eq!(app.view.side_tab, SideTab::FilePreview);
    }

    #[test]
    fn repeated_call_ids_do_not_choose_a_historical_file_or_web_target() {
        let (_temp, mut app) = fixture();
        app.view.transcript.push(app.view.transcript[0].clone());
        click(&mut app, "owner", "call");
        assert!(!app.view.show_side_pane);
        assert!(app.view.file_preview.is_none());
        app.view.transcript.clear();
        for url in ["https://example.com/old", "https://example.com/new"] {
            app.view.transcript.push(TranscriptItem::Tool {
                id: "reused".into(),
                name: "web_fetch".into(),
                arguments: json!({"url":url}),
                output: Some("Captured".into()),
                is_error: false,
                activity: ActivityStatus::Completed,
            });
        }
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        app.apply_tool_navigation_action(
            &UiAction::Clicked {
                id: tool_presentation::navigation_action("owner", "reused"),
            },
            &ctx,
        );
        let mut output = ctx.end_pass();
        assert!(!output
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(command, egui::OutputCommand::OpenUrl(_))));
        output.textures_delta.clear();
    }

    #[test]
    fn file_navigation_rejects_escape_missing_directory_and_unconfirmed_input() {
        let (temp, mut app) = fixture();
        std::fs::write(temp.path().join("outside.rs"), "outside").unwrap();
        for path in ["../outside.rs", "src/missing.rs", "src"] {
            if let TranscriptItem::Tool { arguments, .. } = &mut app.view.transcript[0] {
                *arguments = json!({"path":path});
            }
            click(&mut app, "owner", "call");
            assert!(!app.view.show_side_pane, "{path}");
        }
        app.view.transcript = vec![TranscriptItem::ToolPreview {
            id: "preview".into(),
            step: 1,
            item_id: "item".into(),
            call_id: Some("call".into()),
            name: Some("read_file".into()),
            arguments: r#"{"path":"src/main.rs"}"#.into(),
            input_complete: true,
            status: ModelStreamStatus::Receiving,
            truncated: false,
        }];
        click(&mut app, "owner", "call");
        assert!(!app.view.show_side_pane);
    }

    #[test]
    fn web_navigation_emits_only_confirmed_http_links_as_platform_output() {
        let (_temp, mut app) = fixture();
        for (url, accepted) in [
            ("https://example.test/reference", true),
            ("file:///etc/passwd", false),
            ("javascript:alert(1)", false),
            ("https://host/\nnext", false),
        ] {
            app.view.transcript[0] = TranscriptItem::Tool {
                id: "web".into(),
                name: "mcp__openide_browser__browser_navigate".into(),
                arguments: json!({"url":url}),
                output: None,
                is_error: false,
                activity: ActivityStatus::Running,
            };
            let ctx = egui::Context::default();
            ctx.begin_pass(egui::RawInput::default());
            app.apply_tool_navigation_action(
                &UiAction::Clicked {
                    id: tool_presentation::navigation_action("owner", "web"),
                },
                &ctx,
            );
            let mut output = ctx.end_pass();
            let opened = output
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command, egui::OutputCommand::OpenUrl(_)));
            output.textures_delta.clear();
            assert_eq!(opened, accepted, "{url}");
        }
    }

    fn media_fixture() -> (
        tempfile::TempDir,
        AgentsWindow,
        Vec<openide_ui::RasterImage>,
    ) {
        let (temp, mut app) = fixture();
        let images: Vec<_> = [32, 220]
            .into_iter()
            .map(|value| {
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba([value, value, value, 255]),
                ))
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
                let image = openide_core::UserImage::from_bytes(
                    openide_core::UserImageMime::Png,
                    bytes.get_ref(),
                )
                .unwrap();
                openide_ui::RasterImage::new(
                    image.sha256,
                    openide_ui::RasterImageFormat::Png,
                    image.size_bytes,
                    std::sync::Arc::from(image.data_base64),
                    "Captura confirmada",
                )
                .unwrap()
            })
            .collect();
        app.view.transcript.push(TranscriptItem::ToolMedia {
            tool_call_id: "call".into(),
            images: images
                .iter()
                .map(|image| crate::transcript::TranscriptImage {
                    raster: Some(image.clone()),
                    alt: "Captura confirmada".into(),
                })
                .collect(),
            text: String::new(),
            notices: Vec::new(),
        });
        (temp, app, images)
    }

    #[test]
    fn exact_typed_image_opens_and_navigates_the_native_viewer_without_a_worker() {
        let (_temp, mut app, images) = media_fixture();
        app.attachment_worker = None;
        assert!(app.attachment_worker.is_none());
        let ctx = egui::Context::default();
        let generation = app.attachment_viewer_generation;
        assert!(app.apply_tool_navigation_action(
            &UiAction::Clicked {
                id: tool_presentation::image_action("owner", "call", 1, images[1].sha256()),
            },
            &ctx
        ));
        let gallery = app.view.composer_image_viewer.as_ref().unwrap();
        assert!(gallery.attachments.is_empty());
        assert!(!gallery.loading);
        assert_eq!(gallery.generation, generation.wrapping_add(1));
        assert_eq!(gallery.selected, 1);
        assert_eq!(gallery.image_count(), 2);
        assert_eq!(gallery.image.as_ref(), Some(&images[1]));
        let frame =
            openide_ui::UiTree::new(crate::view::build_window_view(&app.view, 900.0, 800.0))
                .layout(openide_ui::Size::new(900.0, 800.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            openide_ui::DrawCommand::RasterImage { image, .. }
                if image.sha256() == images[1].sha256() && image.is_full_resolution())));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_image_close"));
        assert!(app.navigate_composer_images(-1, &ctx));
        let gallery = app.view.composer_image_viewer.as_ref().unwrap();
        assert_eq!(gallery.image.as_ref(), Some(&images[0]));
        assert!(!gallery.loading);
        assert!(gallery.error.is_none());
        assert!(!app.navigate_composer_images(-1, &ctx));
        app.view.selected_session = Some("another-owner".into());
        let frame =
            openide_ui::UiTree::new(crate::view::build_window_view(&app.view, 900.0, 800.0))
                .layout(openide_ui::Size::new(900.0, 800.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "composer_image_close"));
        app.sync_stream_presentation(std::time::Instant::now());
        assert!(
            app.view.composer_image_viewer.is_none(),
            "stale gallery must release modal input"
        );
    }

    #[test]
    fn tool_image_click_rejects_stale_owner_call_index_digest_and_detached_media() {
        let (_temp, mut app, images) = media_fixture();
        for (owner, call, index, digest) in [
            ("previous-owner", "call", 0, images[0].sha256()),
            ("owner", "previous-call", 0, images[0].sha256()),
            ("owner", "call", 9, images[0].sha256()),
            ("owner", "call", 0, images[1].sha256()),
        ] {
            assert!(app.apply_tool_navigation_action(
                &UiAction::Clicked {
                    id: tool_presentation::image_action(owner, call, index, digest),
                },
                &egui::Context::default()
            ));
            assert!(app.view.composer_image_viewer.is_none());
        }
        if let TranscriptItem::ToolMedia { tool_call_id, .. } = &mut app.view.transcript[1] {
            *tool_call_id = "detached-call".into();
        }
        app.apply_tool_navigation_action(
            &UiAction::Clicked {
                id: tool_presentation::image_action("owner", "call", 0, images[0].sha256()),
            },
            &egui::Context::default(),
        );
        assert!(app.view.composer_image_viewer.is_none());
    }
}
