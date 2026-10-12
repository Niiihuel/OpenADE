//! One-way observations for explicitly marked desktop fixtures only. Never
//! compiled in ordinary builds and never accepts actions from the filesystem.
//! Real keyboard/pointer events remain the only E2E input path.

use super::*;
use serde_json::{json, Value};
use std::sync::mpsc::{self, SyncSender};

pub(super) struct Observer {
    sender: SyncSender<Value>,
    previous: Option<Value>,
    usage_fixture: bool,
    marketplace_fixture: bool,
    menu_fixture: bool,
}

impl Observer {
    pub(super) fn new(root: &Path) -> Self {
        assert!(root.join(".openide-editor-fixture").is_file());
        let destination = root.join("native-state.json");
        let temporary = root.join(".native-state.json.tmp");
        let input_trace = root.join("native-input.jsonl");
        let (sender, receiver) = mpsc::sync_channel::<Value>(1);
        std::thread::Builder::new()
            .name("desktop-e2e-observer".into())
            .spawn(move || {
                while let Ok(value) = receiver.recv() {
                    if value.get("raw_input").and_then(Value::as_str).is_some_and(|text| text != "[]") {
                        use std::io::Write;
                        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&input_trace) {
                            let trace = json!({"raw": value["raw_input"], "mapped": value["mapped_input"], "focus": value["focused"]});
                            let _ = writeln!(file, "{}", trace);
                        }
                    }
                    let Ok(bytes) = serde_json::to_vec(&value) else {
                        continue;
                    };
                    if std::fs::write(&temporary, bytes).is_ok() {
                        let _ = std::fs::rename(&temporary, &destination);
                    }
                }
            })
            .expect("fixture observer worker");
        Self {
            sender,
            previous: None,
            usage_fixture: std::env::var("OPENIDE_USAGE_DESKTOP_FIXTURE").as_deref() == Ok("1")
                && root.join("usage-fixture.json").is_file(),
            marketplace_fixture: std::env::var("OPENIDE_MARKETPLACE_DESKTOP_FIXTURE").as_deref()
                == Ok("1")
                && root.join("marketplace-fixture.json").is_file(),
            menu_fixture: std::env::var("OPENIDE_MENU_DESKTOP_FIXTURE").as_deref() == Ok("1")
                && root.join("menu-fixture.json").is_file(),
        }
    }

    fn submit(&mut self, value: Value) {
        if self.previous.as_ref() == Some(&value) {
            return;
        }
        if self.sender.try_send(value.clone()).is_ok() {
            self.previous = Some(value);
        }
    }
}

impl AgentsWindow {
    /// Open the real Marketplace worker over a private, empty data root.
    /// The native fixture adds and installs its local skill through pointer/key
    /// input; this hook never preinstalls skills or downloads a source.
    pub(super) fn prepare_marketplace_e2e(&mut self, root: &Path) {
        if std::env::var("OPENIDE_MARKETPLACE_DESKTOP_FIXTURE").as_deref() != Ok("1")
            || !root.join("marketplace-fixture.json").is_file()
        {
            return;
        }
        assert!(root.join(".openide-editor-fixture").is_file());
        assert!(!root.join("state/provider-credentials").exists());
        let dormant_until = Instant::now() + Duration::from_secs(3_600);
        self.provider_usage_worker = None;
        self.view.provider_usage_refreshing = false;
        self.provider_usage_last_refresh = dormant_until;
        self.provider_usage_last_identity_check = dormant_until;
        self.provider_refresh_last_poll = dormant_until;
        self.catalog_response = None;
        self.catalog_last_refresh = dormant_until;
        self.view.catalog_refreshing = false;
        self.view.main_page = MainPage::Marketplace;
        self.view.show_settings = false;
        self.view.sidebar_collapsed = true;
        self.editor_services.focus_next = None;
        self.marketplace_operation(MarketplaceOperation::Load);
    }
    /// Synthetic visual data for the explicitly opted-in private Usage fixture.
    /// No credentials are created and no provider request is started here.
    pub(super) fn prepare_usage_e2e(&mut self, root: &Path) {
        if std::env::var("OPENIDE_USAGE_DESKTOP_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let fixture = root.join("usage-fixture.json");
        if !fixture.is_file() {
            return;
        }
        assert!(root.join(".openide-editor-fixture").is_file());
        let bytes = std::fs::read(&fixture).expect("Usage fixture configuration");
        assert!(bytes.len() <= 256);
        let config: Value = serde_json::from_slice(&bytes).expect("Usage fixture JSON");
        let empty = config["empty"]
            .as_bool()
            .expect("explicit empty chart fixture");
        assert!(!root.join("state/provider-credentials").exists());
        self.provider_usage_worker = None;
        self.view.provider_usage_refreshing = false;
        // The fixture has no account endpoint. Keep background identity/usage
        // polling dormant while exercising the actual retained UI and routing.
        let dormant_until = Instant::now() + Duration::from_secs(3_600);
        self.provider_usage_last_refresh = dormant_until;
        self.provider_usage_last_identity_check = dormant_until;
        self.provider_refresh_last_poll = dormant_until;
        self.catalog_response = None;
        self.catalog_last_refresh = dormant_until;
        self.view.catalog_refreshing = false;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        use crate::provider_usage::{
            ProviderUsageSnapshot, ProviderUsageStatus, UsageUnit, UsageWindow,
        };
        self.view.provider_usage.clear();
        for (id, name, _) in crate::provider_connections::PROVIDERS {
            let configured = matches!(*id, "codex" | "openrouter" | "opencode");
            self.view.provider_connections.insert(
                (*id).into(),
                crate::provider_connections::ConnectionSummary {
                    configured,
                    account: configured.then(|| "cuenta.sintetica@ejemplo.invalid".into()),
                    ..Default::default()
                },
            );
            let mut snapshot = ProviderUsageSnapshot {
                provider_id: (*id).into(),
                provider_name: (*name).into(),
                status: if configured {
                    ProviderUsageStatus::External
                } else {
                    ProviderUsageStatus::Disconnected
                },
                plan_label: None,
                account: configured.then(|| "cuenta.sintetica@ejemplo.invalid".into()),
                windows: vec![],
                balances: vec![],
                fetched_at_ms: None,
                local: false,
                account_url: configured.then(|| format!("https://fixture.invalid/account/{id}")),
                message: None,
            };
            match *id {
                "codex" => {
                    snapshot.plan_label = Some("Plan de ChatGPT".into());
                    snapshot.message =
                        Some("Los límites de tu plan se consultan en ChatGPT.".into());
                }
                "openrouter" => {
                    snapshot.status = ProviderUsageStatus::Ready;
                    snapshot.plan_label = Some("Créditos de API".into());
                    snapshot.fetched_at_ms = Some(now);
                    snapshot.windows.push(UsageWindow {
                        label: "Límite de la clave".into(),
                        used: 3.0,
                        limit: Some(10.0),
                        remaining: Some(7.0),
                        unit: UsageUnit::Usd,
                        reset_at_ms: None,
                        reset_label: None,
                    });
                }
                "opencode" => {
                    snapshot.status = ProviderUsageStatus::Ready;
                    snapshot.plan_label = Some("OpenCode Go".into());
                    snapshot.fetched_at_ms = Some(now);
                    snapshot.windows = [
                        ("Sesión · 5 h", 82.0, 5_400_000),
                        ("Semanal", 36.0, 345_600_000),
                    ]
                    .into_iter()
                    .map(|(label, used, remaining_ms)| UsageWindow {
                        label: label.into(),
                        used,
                        limit: Some(100.0),
                        remaining: Some(100.0 - used),
                        unit: UsageUnit::Percent,
                        reset_at_ms: Some(now + remaining_ms),
                        reset_label: None,
                    })
                    .collect();
                }
                _ => {}
            }
            self.view.provider_usage.insert((*id).into(), snapshot);
        }
        if !empty {
            for backend in ["native", "native", "native", "codex", "codex", "claude"] {
                self.store
                    .as_ref()
                    .expect("fixture store")
                    .create(Path::new(&self.view.workspace), backend)
                    .expect("synthetic task history");
            }
        }
        self.refresh_sessions();
        self.view.backend_options = vec![
            ("native".into(), "Agente nativo".into()),
            ("codex".into(), "Codex".into()),
            (
                "claude".into(),
                "Agente con un nombre deliberadamente largo".into(),
            ),
        ];
        if config["catalog"].as_bool() == Some(true) {
            // This initial account listing is deliberately synthetic. Its fake
            // credential identity differs from the credential-free fixture so
            // an actual refresh must invalidate it instead of claiming success.
            let mut catalog = crate::model_catalog::load_cache(&self.data_root)
                .expect("fixture catalog seed")
                .expect("fixture catalog");
            let provider = catalog
                .providers
                .get_mut("codex")
                .expect("Codex registry source");
            provider.models.clear();
            for (index, (id, name)) in [
                ("gpt-6-astra", "GPT-6 Astra"),
                ("gpt-5.6-sol", "GPT-5.6 Sol"),
                ("gpt-5.6-terra", "GPT-5.6 Terra"),
                ("gpt-5.6-luna", "GPT-5.6 Luna"),
            ]
            .into_iter()
            .enumerate()
            {
                let model = serde_json::from_value(json!({
                    "id": id, "name": name, "listing_index": index,
                    "tool_call": true, "tool_call_declared": true,
                    "reasoning": true, "attachment": true,
                }))
                .expect("synthetic Codex model");
                provider.models.insert(id.into(), model);
            }
            let status = catalog
                .provider_status
                .get_mut("codex")
                .expect("Codex status");
            status.source = crate::model_catalog::CatalogSource::OfficialApi;
            status.verified_at_ms = now.saturating_sub(3_600_000);
            status.attempted_at_ms = status.verified_at_ms;
            status.credential_fingerprint = Some("f".repeat(64));
            status.last_error = None;
            catalog.fetched_at_ms = status.verified_at_ms;
            self.view.catalog = Some(Arc::new(catalog));
            self.view.native_provider_id = "codex".into();
            self.view.model = "gpt-5.6-luna".into();
        }
        self.view.show_settings = true;
        self.view.settings_page = SettingsPage::Usage;
        self.view.settings_nav_open = false;
        self.editor_services.focus_next = None;
    }

    /// Optional bootstrap for the native Context UI fixture. Only a marked
    /// private fixture may configure the loopback-only, synthetic provider.
    /// Subsequent interaction and measurements go through real UI/HTTP paths.
    pub(super) fn prepare_context_e2e(&mut self, root: &Path) {
        let fixture = root.join("context-fixture.json");
        if !fixture.is_file() {
            return;
        }
        assert!(root.join(".openide-editor-fixture").is_file());
        let bytes = std::fs::read(&fixture).expect("context fixture configuration");
        assert!(bytes.len() <= 4096);
        let config: Value = serde_json::from_slice(&bytes).expect("context fixture JSON");
        let endpoint = reqwest::Url::parse(config["endpoint"].as_str().expect("fixture endpoint"))
            .expect("fixture endpoint URL");
        assert_eq!(endpoint.scheme(), "http");
        assert_eq!(endpoint.host_str(), Some("127.0.0.1"));
        assert!(endpoint.port().is_some());
        assert!(endpoint.username().is_empty() && endpoint.password().is_none());
        assert!(endpoint.query().is_none() && endpoint.fragment().is_none());
        assert_eq!(endpoint.path(), "/v1");
        let model = "context-desktop-fixture";
        self.view.selected_backend = "native".into();
        self.view.native_provider_id = "custom".into();
        self.view.model = model.into();
        self.view.composer_model = model.into();
        self.view.composer_effort = "default".into();
        self.view.endpoint = endpoint.to_string();
        self.model_discoveries = vec![native_provider::ProviderModelDiscovery {
            provider_id: "custom".into(),
            models: BTreeMap::from([(
                model.into(),
                native_provider::DiscoveredModel {
                    listing_index: None,
                    limits_declared: false,
                    efforts_declared: false,
                    capabilities_declared: false,
                    max_input_tokens: None,
                    declared_capabilities: Default::default(),
                    id: model.into(),
                    name: "Contexto · proveedor local de prueba".into(),
                    provider_reports_tools: true,
                    tool_call_declared: Some(true),
                    text_input_output: true,
                    supported_efforts: vec![],
                    context_window_tokens: Some(20_000),
                    max_output_tokens: Some(4_000),
                },
            )]),
        }];
        assert!(
            self.persist_native_settings(),
            "fixture settings must persist"
        );
        let session = self
            .store
            .as_ref()
            .expect("fixture session store")
            .create(Path::new(&self.view.workspace), "native")
            .expect("context fixture session");
        self.open_session(&session.id);
        // The production account picker intentionally filters the legacy
        // `custom` adapter. This fixture explicitly configures a loopback
        // provider and derives its selectable route from the real validator.
        self.view.composer_routes = composer_selection::model_route_options_with_discovery_at(
            None,
            &self.native_settings(),
            &self.model_discoveries,
            &self.data_root,
        )
        .expect("validated local fixture routes")
        .into_iter()
        .filter(|route| route.provider_id == "custom" && route.model_id == model)
        .collect();
        assert!(self
            .view
            .composer_routes
            .iter()
            .any(|route| route.selectable()));
        self.view.composer_model_metadata = crate::model_metadata::for_routes(
            &self.view.composer_routes,
            None,
            &self.model_discoveries,
            &[],
        );
        self.refresh_native_composer_choice_metadata();
        // Do not let an unrelated public catalog refresh replace the isolated
        // fixture picker. No provider credentials or catalog are required.
        self.catalog_response = None;
        self.catalog_last_refresh = Instant::now();
        self.view.catalog_refreshing = false;
        self.view.turn_mode = TurnMode::Plan;
        let scope = self.dock_scope();
        self.dock.set_collapsed(&scope, true);
        self.sync_dock_view();
        self.editor_services.focus_next = None;
        self.refresh_composer_context_percent();
    }

    pub(super) fn observe_editor_e2e(&mut self, ctx: &egui::Context) {
        if self.editor_services.e2e_observer.is_none() {
            return;
        }
        // Observe the last painted frame even if the fixture becomes idle.
        // No filesystem I/O or blocking send occurs on the UI thread.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        let frame = self.tree.frame();
        let usage_fixture = self
            .editor_services
            .e2e_observer
            .as_ref()
            .unwrap()
            .usage_fixture;
        let marketplace_fixture = self
            .editor_services
            .e2e_observer
            .as_ref()
            .unwrap()
            .marketplace_fixture;
        let menu_fixture = self.editor_services.e2e_observer.as_ref().unwrap().menu_fixture;
        let usage_paint: Vec<_> = if usage_fixture || marketplace_fixture || menu_fixture {
            frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    openide_ui::DrawCommand::FillRect {
                        rect,
                        color,
                        radius,
                    } => Some(json!({
                        "kind": "rect",
                        "rect": [rect.x, rect.y, rect.width, rect.height],
                        "color": [color.r, color.g, color.b, color.a], "radius": radius,
                    })),
                    openide_ui::DrawCommand::StrokeRect {
                        rect,
                        color,
                        radius,
                        ..
                    } => Some(json!({
                        "kind": "stroke",
                        "rect": [rect.x, rect.y, rect.width, rect.height],
                        "color": [color.r, color.g, color.b, color.a], "radius": radius,
                    })),
                    openide_ui::DrawCommand::FillCornerRect { rect, color, .. } => Some(json!({
                        "kind": "corner",
                        "rect": [rect.x, rect.y, rect.width, rect.height],
                        "color": [color.r, color.g, color.b, color.a], "radius": 0,
                    })),
                    _ => None,
                })
                .take(512)
                .collect()
        } else {
            vec![]
        };
        let regions: std::collections::BTreeMap<_, _> = frame
            .regions
            .iter()
            .rev() // Keep the earlier scroll viewport, not its later scrollbar rail.
            .take(1024)
            .map(|region| {
                let rect = region.rect;
                let center =
                    openide_ui::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                (
                    region.id.clone(),
                    json!({
                        "rect": [rect.x, rect.y, rect.width, rect.height],
                        "hit": frame.hit_test(center),
                        "kind": format!("{:?}", region.kind),
                    }),
                )
            })
            .collect();
        let texts: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                openide_ui::DrawCommand::Text { text, .. } => {
                    Some(text.chars().take(512).collect::<String>())
                }
                _ => None,
            })
            .take(512)
            .collect();
        let tabs = |tabs: &[crate::view::DockTabView]| {
            tabs.iter().map(|tab| json!({
            "id": tab.id, "title": tab.title, "kind": format!("{:?}", tab.kind), "active": tab.active,
        })).collect::<Vec<_>>()
        };
        let snapshot = self.view.terminal_snapshot.as_ref();
        let dock_layout = crate::view::dock_layout(&self.view, self.viewport_width);
        let context_rings: Vec<_> = frame.commands.iter().filter_map(|command| {
            if let openide_ui::DrawCommand::ProgressRing { rect, fraction, .. } = command {
                Some(json!({"rect": [rect.x, rect.y, rect.width, rect.height], "fraction": fraction}))
            } else {
                None
            }
        }).collect();
        let editor = self.active_editor().map(|(tab, path, _, snapshot)| {
            let state = &self.view.editor_workbench;
            let mut source = snapshot.text_range(0..snapshot.char_count().min(32_768)).unwrap_or_default();
            if source.len() > 32_768 {
                let mut end = 32_768;
                while !source.is_char_boundary(end) { end -= 1; }
                source.truncate(end);
            }
            let caret = snapshot.position_of(snapshot.selection().head).ok();
            let breakpoint_glyphs: std::collections::BTreeMap<_, _> = frame.regions.iter()
                .filter(|region| region.id.starts_with("editor_breakpoint:"))
                .take(96)
                .map(|region| {
                    let glyphs: Vec<_> = frame.commands.iter().filter_map(|command| {
                        let (kind, rect, color, radius) = match command {
                            openide_ui::DrawCommand::FillRect { rect, color, radius } => ("fill", rect, color, radius),
                            openide_ui::DrawCommand::StrokeRect { rect, color, radius, .. } => ("stroke", rect, color, radius),
                            _ => return None,
                        };
                        (rect.width <= 12.0 && rect.height <= 12.0
                            && rect.x >= region.rect.x && rect.y >= region.rect.y
                            && rect.x + rect.width <= region.rect.x + region.rect.width
                            && rect.y + rect.height <= region.rect.y + region.rect.height)
                            .then(|| json!({"kind": kind, "rect": [rect.x, rect.y, rect.width, rect.height],
                                "color": [color.r, color.g, color.b, color.a], "radius": radius}))
                    }).take(4).collect();
                    (region.id.clone(), glyphs)
                }).collect();
            json!({
                "tab": tab, "path": path, "document": snapshot.document_id(),
                "revision": snapshot.revision(), "dirty": snapshot.is_dirty(),
                "chars": snapshot.char_count(), "line_count": snapshot.line_count(),
                "text_truncated": source.chars().count() < snapshot.char_count(), "text": source,
                "caret_row": caret.map(|position| position.row),
                "caret_column": caret.map(|position| position.column),
                "selection": [snapshot.selection().anchor, snapshot.selection().head],
                "vim_mode": self.tree.code_editor_vim_mode(&crate::code_editor_ui::view_id(tab))
                    .map(|mode| format!("{mode:?}")),
                "structure_revision": state.structure_revision,
                "folds": state.folds.iter().take(256).map(|range| [range.start, range.end]).collect::<Vec<_>>(),
                "fold_candidates": state.fold_candidates.iter().take(256).map(|fold| json!({
                    "id": fold.action_id, "start": fold.start_row, "end": fold.end_row,
                })).collect::<Vec<_>>(),
                "sticky_scopes": state.sticky_scopes.iter().take(256).map(|scope| json!({
                    "id": scope.action_id, "start": scope.start_row, "end": scope.end_row,
                })).collect::<Vec<_>>(),
                "breakpoint_revision": state.breakpoint_revision,
                "breakpoints": state.breakpoint_rows.iter().take(256).copied().collect::<Vec<_>>(),
                "breakpoint_glyphs": breakpoint_glyphs,
                "debug_breakpoints": state.debug_breakpoints.iter().take(256).map(|marker| json!({
                    "row": marker.row, "state": format!("{:?}", marker.state),
                    "message": marker.message.as_ref().map(|value| value.chars().take(256).collect::<String>()),
                })).collect::<Vec<_>>(),
                "debug_execution": state.debug_execution.map(|line| json!({"row": line.row, "primary": line.primary})),
            })
        });
        let marketplace_observation = if marketplace_fixture { json!({
                "page": format!("{:?}", self.view.main_page),
                "query": self.view.marketplace.query,
                "active_query": self.view.marketplace.query,
                "filter": self.view.marketplace.filter.key(),
                "category": self.view.marketplace.category,
                "visible": self.view.marketplace.visible,
                "explore": {
                    "query": self.view.marketplace.explore.query,
                    "busy": self.view.marketplace.explore.busy,
                    "error": self.view.marketplace.explore.error.as_ref().map(|value| value.chars().take(512).collect::<String>()),
                    "results": self.view.marketplace.explore.page.as_ref().map(|page| json!({
                        "query": page.query, "from_cache": page.from_cache, "stale": page.stale,
                        "warning": page.warning.as_ref().map(|value| value.chars().take(512).collect::<String>()),
                        "skills": page.skills.iter().take(100).map(|skill| json!({
                            "id": skill.id, "name": skill.name, "source": skill.source, "installs": skill.installs,
                        })).collect::<Vec<_>>(),
                    })),
                },
                "update_checking": self.view.marketplace.update_checking,
                "updates_checked": self.view.marketplace.updates_checked,
                "update_error": self.view.marketplace.update_error.as_ref().map(|value| value.chars().take(512).collect::<String>()),
                "updates": self.view.marketplace.updates.iter().take(64).map(|(id, update)| json!({
                    "id": id, "installed": update.installed_revision, "available": update.available_revision,
                })).collect::<Vec<_>>(),
                "provenance": self.view.marketplace.provenance.iter().take(64).map(|(id, value)| json!({
                    "id": id, "repository": value.repository, "revision": value.revision, "directory": value.directory,
                })).collect::<Vec<_>>(),
                "installed_only": self.view.marketplace.filter == crate::marketplace_view::MarketplaceFilter::Installed,
                "scope": format!("{:?}", self.view.marketplace.install_scope),
                "busy": self.view.marketplace.busy,
                "status": self.view.marketplace.status,
                "status_error": self.view.marketplace.status_error,
                "add_menu_open": self.view.marketplace.add_menu_open,
                "source_kind": self.view.marketplace.add_source_kind.map(|kind| format!("{kind:?}")),
                "source_draft": self.view.marketplace.source_draft,
                "selected_item": self.view.marketplace.selected_item_id,
                "pending_uninstall": self.view.marketplace.pending_uninstall_id,
                "pending_remove_source": self.view.marketplace.pending_remove_source_id,
                "scroll": self.tree.scroll_offset("marketplace_page"),
                "sources": self.view.marketplace.snapshot.sources.iter().take(32).map(|source| json!({
                    "id": source.id, "name": source.name, "kind": format!("{:?}", source.kind),
                })).collect::<Vec<_>>(),
                "items": self.view.marketplace.snapshot.items.iter().take(128).map(|item| json!({
                    "id": item.id, "name": item.name, "source": item.source_name,
                    "installed_scope": item.installed_scope.map(|scope| format!("{scope:?}")),
                    "enabled": item.enabled, "source_available": item.source_available,
                })).collect::<Vec<_>>(),
                "diagnostics": self.view.marketplace.snapshot.diagnostics.iter().take(16)
                    .map(|value| value.chars().take(512).collect::<String>()).collect::<Vec<_>>(),
                "paint": usage_paint,
                "labels": frame.commands.iter().filter_map(|command| match command {
                    openide_ui::DrawCommand::Text { text, position, font_size, font_family, .. } => {
                        let font = match font_family {
                            openide_ui::FontFamily::Sans | openide_ui::FontFamily::SansItalic | openide_ui::FontFamily::SansMedium | openide_ui::FontFamily::SansBold => egui::FontId::proportional(*font_size),
                            openide_ui::FontFamily::Monospace => egui::FontId::monospace(*font_size),
                        };
                        let measured = ctx.fonts_mut(|fonts| fonts.layout_no_wrap(text.clone(), font, egui::Color32::WHITE).size());
                        Some(json!({"text": text.chars().take(160).collect::<String>(),
                            "position": [position.x, position.y], "size": font_size,
                            "bounds": [measured.x, measured.y]}))
                    },
                    _ => None,
                }).take(512).collect::<Vec<_>>(),
            }) } else { Value::Null };
        let value = json!({
            "raw_input": ctx.input(|input| format!("{:?}", input.events)),
            "mapped_input": format!("{:?}", crate::render::events(ctx, egui::Pos2::ZERO)),
            "pid": std::process::id(),
            "workspace": self.view.workspace,
            "ide": self.view.ide.is_active(),
            "viewport": [self.viewport_width, self.viewport_height],
            "regions": regions,
            "duplicate_region_ids": if marketplace_fixture {
                let mut counts = std::collections::BTreeMap::<&str, (usize, usize, usize)>::new();
                for region in frame.regions.iter().take(4096) {
                    let count = counts.entry(region.id.as_str()).or_default();
                    count.0 += 1;
                    count.1 += usize::from(region.kind == openide_ui::HitKind::Scroll);
                    count.2 += usize::from(region.kind == openide_ui::HitKind::Scrollbar);
                }
                counts.into_iter().filter(|(_, count)| count.0 > 1 && *count != (2, 1, 1))
                    .take(64).map(|(id, count)| json!({"id": id, "count": count.0})).collect::<Vec<_>>()
            } else { vec![] },
            "texts": texts,
            "focused": self.tree.focused_id(),
            "menus": if menu_fixture { json!({
                "open": self.view.ide.menu_open,
                "hovered": self.tree.hovered_id(),
                "context_open": self.tree.context_menu_open(),
                "activity": self.view.ide.activity.key(),
                "explorer_open": self.view.ide.explorer_open,
                "agent_open": self.view.ide.agent_open,
                "new_file": self.view.ide.new_file.is_some(),
                "command_center": self.view.show_command_center,
                "command_scope": format!("{:?}", self.view.command_scope),
                "command_focused": self.command_tree.focused_id(),
                "command_query": self.view.command_query.chars().take(512).collect::<String>(),
                "settings_open": self.view.show_settings,
                "settings_page": format!("{:?}", self.view.settings_page),
                "keymap": self.view.keymap_profile.id(),
                "vim_enabled": self.view.vim_mode,
                "bottom_open": self.view.ide.bottom_open,
                "bottom_panel": format!("{:?}", self.view.ide.bottom_panel),
                "model_picker": self.view.composer_model_menu_open,
                "images": frame.commands.iter().filter_map(|command| match command {
                    openide_ui::DrawCommand::RasterImage { image, rect, .. } => Some(json!({
                        "alt": image.alt(), "rect": [rect.x, rect.y, rect.width, rect.height],
                    })),
                    _ => None,
                }).take(32).collect::<Vec<_>>(),
                "paint": usage_paint,
                "labels": frame.commands.iter().filter_map(|command| match command {
                    openide_ui::DrawCommand::Text { text, position, font_size, font_family, .. } => {
                        let font = match font_family {
                            openide_ui::FontFamily::Sans | openide_ui::FontFamily::SansItalic | openide_ui::FontFamily::SansMedium | openide_ui::FontFamily::SansBold => egui::FontId::proportional(*font_size),
                            openide_ui::FontFamily::Monospace => egui::FontId::monospace(*font_size),
                        };
                        let measured = ctx.fonts_mut(|fonts| fonts.layout_no_wrap(text.clone(), font, egui::Color32::WHITE).size());
                        Some(json!({"text": text.chars().take(160).collect::<String>(),
                            "position": [position.x, position.y], "size": font_size,
                            "bounds": [measured.x, measured.y]}))
                    },
                    _ => None,
                }).take(512).collect::<Vec<_>>(),
            }) } else { Value::Null },
            "editor": editor,
            "marketplace": marketplace_observation,
            "usage": if usage_fixture { json!({
                "settings_open": self.view.show_settings,
                "page": format!("{:?}", self.view.settings_page),
                "scroll": self.tree.scroll_offset("settings_page:Usage"),
                "provider_connect": self.view.provider_connect.as_ref().map(|view| &view.provider_id),
                "worker_running": self.provider_connection_worker.is_some() || self.provider_usage_worker.is_some(),
                "tasks": self.view.sessions.iter().take(64).map(|session| &session.backend).collect::<Vec<_>>(),
                "providers": self.view.provider_usage.keys().take(16).collect::<Vec<_>>(),
                "paint": usage_paint,
                "labels": frame.commands.iter().filter_map(|command| match command {
                    openide_ui::DrawCommand::Text { text, position, font_size, font_family, .. } => {
                        let font = match font_family {
                            openide_ui::FontFamily::Sans | openide_ui::FontFamily::SansItalic | openide_ui::FontFamily::SansMedium | openide_ui::FontFamily::SansBold => egui::FontId::proportional(*font_size),
                            openide_ui::FontFamily::Monospace => egui::FontId::monospace(*font_size),
                        };
                        let measured = ctx.fonts_mut(|fonts| fonts.layout_no_wrap(text.clone(), font, egui::Color32::WHITE).size());
                        Some(json!({
                            "text": text.chars().take(160).collect::<String>(),
                            "position": [position.x, position.y], "size": font_size,
                            "bounds": [measured.x, measured.y],
                        }))
                    },
                    _ => None,
                }).take(512).collect::<Vec<_>>(),
                "catalog": {
                    "provider": self.view.catalog_browse_provider,
                    "selected_provider": self.view.native_provider_id,
                    "selected_model": self.view.model,
                    "refreshing": self.view.catalog_refreshing,
                    "status": self.view.catalog.as_ref().and_then(|catalog| catalog.provider_status.get("codex")).map(|status| json!({
                        "source": format!("{:?}", status.source),
                        "verified_at_ms": status.verified_at_ms,
                        "attempted_at_ms": status.attempted_at_ms,
                        "last_error": status.last_error,
                    })),
                    "models": self.view.catalog.as_ref().and_then(|catalog| catalog.providers.get("codex"))
                        .map(|provider| provider.models.values().take(32).map(|model| json!({
                            "id": model.id, "name": model.name, "listing_index": model.listing_index,
                        })).collect::<Vec<_>>()),
                },
            }) } else { Value::Null },
            "debug": {
                "open": self.view.debug_open,
                "scope": self.view.debug.scope,
                "status": format!("{:?}", self.view.debug.status),
                "configuration": self.view.debug.configuration,
                "adapter": self.view.debug.adapter,
                "message": self.view.debug.message.as_ref().map(|value| value.chars().take(512).collect::<String>()),
                "controls": {
                    "start": self.view.debug.controls.start, "resume": self.view.debug.controls.resume,
                    "pause": self.view.debug.controls.pause, "step_over": self.view.debug.controls.step_over,
                    "step_into": self.view.debug.controls.step_into, "step_out": self.view.debug.controls.step_out,
                    "stop": self.view.debug.controls.stop,
                },
                "threads": self.view.debug.threads.iter().take(64).map(|thread| json!({
                    "id": thread.id, "name": thread.name.chars().take(128).collect::<String>(),
                    "selected": thread.selected, "stopped": thread.stopped,
                })).collect::<Vec<_>>(),
                "frames": self.view.debug.frames.iter().take(64).map(|frame| json!({
                    "id": frame.id, "name": frame.name.chars().take(256).collect::<String>(),
                    "path": frame.path, "line": frame.line, "selected": frame.selected,
                })).collect::<Vec<_>>(),
                "variables": self.view.debug.variables.iter().take(64).map(|variable| json!({
                    "key": variable.key, "name": variable.name.chars().take(128).collect::<String>(),
                    "value": variable.value.chars().take(256).collect::<String>(),
                    "has_children": variable.has_children, "expanded": variable.expanded,
                    "loading": variable.loading,
                })).collect::<Vec<_>>(),
                "output": self.view.debug.output.iter().rev().take(16).map(|output| json!({
                    "category": output.category, "text": output.text.chars().take(512).collect::<String>(),
                })).collect::<Vec<_>>(),
            },
            "context": {
                "open": self.view.composer_context_open,
                "status": format!("{:?}", self.view.native_context.measurement_status),
                "used": self.view.native_context.used_tokens(),
                "capacity": self.view.native_context.capacity(),
                "fraction": self.view.native_context.used_fraction(),
                "reported_responses": self.view.native_context.reported_responses,
                "summary_scroll": self.tree.scroll_offset("composer_context_summary"),
                "rings": context_rings,
                "running": self.view.running,
            },
            "files": self.view.file_rows.iter().take(1024).enumerate().map(|(index, row)| json!({
                "id": format!("file_row:{index}"), "name": row.name, "directory": row.is_dir,
            })).collect::<Vec<_>>(),
            "dock": {
                "tabs": tabs(&self.view.dock_tabs), "width": dock_layout.right,
                "separator_x": dock_layout.right_split_x, "sidebar_width": dock_layout.left,
            },
            "terminal": {
                "visible": self.view.show_terminal_panel,
                "height": crate::view::terminal_panel_height(&self.view, self.viewport_height),
                "tabs": tabs(&self.view.terminal_tabs),
                "running": snapshot.is_some_and(|snapshot| snapshot.running),
                "text": snapshot.map(|snapshot| snapshot.lines.join("\n").chars().take(32_768).collect::<String>()),
                "selection": snapshot.and_then(|snapshot| snapshot.selection.as_ref())
                    .map(|text| text.chars().take(32_768).collect::<String>()),
                "panes": self.view.terminal_controls.panes.iter().take(2).map(|pane| {
                    let id = format!("terminal_screen:{}", pane.id);
                    let region = frame.regions.iter().find(|region| region.id == id);
                    let active = self.view.terminal_tabs.iter().any(|tab| tab.id == pane.id && tab.active);
                    let snapshot = if active { self.view.terminal_snapshot.as_ref() } else { pane.snapshot.as_ref() };
                    let error = if active { self.view.terminal_error.as_ref() } else { pane.error.as_ref() };
                    json!({
                        "id": pane.id,
                        "active": active,
                        "rect": region.map(|region| [region.rect.x, region.rect.y, region.rect.width, region.rect.height]),
                        "running": snapshot.is_some_and(|snapshot| snapshot.running),
                        "text": snapshot.map(|snapshot| snapshot.lines.join("\n").chars().take(32_768).collect::<String>()),
                        "error": error,
                    })
                }).collect::<Vec<_>>(),
            },
            "browser": {
                "started": self.view.browser.started,
                "navigating": self.view.browser.navigating,
                "url": self.view.browser.url,
                "error": self.view.browser.error,
                "viewport": [self.view.browser.viewport_width, self.view.browser.viewport_height],
            },
        });
        self.editor_services
            .e2e_observer
            .as_mut()
            .unwrap()
            .submit(value);
    }
}
