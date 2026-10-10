//! Host-owned workers keep discovery, downloads and update checks off the UI thread.
use super::*;
use crate::marketplace::{
    MarketplaceSnapshot, RemoteSearchPage, RemoteSkill, SkillProvenance, SkillUpdate,
};
use crate::marketplace_view::{catalog_page, MarketplaceTab, PAGE_SIZE};
use std::collections::BTreeMap;

pub(super) enum MarketplaceOperation {
    Load,
    Refresh,
    CheckUpdates,
    Update(String),
    AddLocal(PathBuf),
    AddGithub(String),
    Install(String, InstallScope),
    InstallRemote(RemoteSkill, InstallScope),
    SetEnabled(String, bool),
    Uninstall(String),
    RemoveSource(String),
}

pub(super) struct MarketplaceResult {
    snapshot: MarketplaceSnapshot,
    message: String,
    provenance: BTreeMap<String, SkillProvenance>,
    updates: Option<Vec<SkillUpdate>>,
    update_error: Option<String>,
    updated_item: Option<String>,
    checked_updates: bool,
}

pub(super) type MarketplaceResponse = (PathBuf, Result<MarketplaceResult, String>);
type SearchResponse = (PathBuf, u64, String, Result<RemoteSearchPage, String>);

#[derive(Default)]
pub(super) struct MarketplaceRemoteHost {
    response: Option<Receiver<SearchResponse>>,
    requested: Option<(String, bool)>,
    generation: u64,
}

impl MarketplaceRemoteHost {
    pub(super) fn pending(&self) -> bool {
        self.response.is_some() || self.requested.is_some()
    }
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.requested = None;
    }
}

impl AgentsWindow {
    pub(super) fn marketplace_operation(&mut self, operation: MarketplaceOperation) {
        if self.marketplace_response.is_some() {
            return;
        }
        let workspace = PathBuf::from(&self.view.workspace);
        let store = MarketplaceStore::new(self.data_root.clone(), workspace.clone());
        let (sender, receiver) = mpsc::channel();
        let check_updates = matches!(
            operation,
            MarketplaceOperation::CheckUpdates | MarketplaceOperation::Refresh
        );
        self.view.marketplace.busy = true;
        self.view.marketplace.update_checking = check_updates;
        self.view.marketplace.status_error = false;
        if check_updates {
            self.view.marketplace.update_error = None;
        }
        self.view.marketplace.status = Some(
            match &operation {
                MarketplaceOperation::Load => "Cargando catálogo…",
                MarketplaceOperation::Refresh => "Actualizando orígenes…",
                MarketplaceOperation::CheckUpdates => "Buscando actualizaciones…",
                MarketplaceOperation::Update(_) => "Descargando actualización…",
                MarketplaceOperation::AddLocal(_) => "Leyendo carpeta…",
                MarketplaceOperation::AddGithub(_) => "Descargando origen…",
                MarketplaceOperation::Install(..) | MarketplaceOperation::InstallRemote(..) => {
                    "Descargando e instalando skill…"
                }
                MarketplaceOperation::SetEnabled(_, true) => "Activando skill…",
                MarketplaceOperation::SetEnabled(_, false) => "Desactivando skill…",
                MarketplaceOperation::Uninstall(_) => "Desinstalando skill…",
                MarketplaceOperation::RemoveSource(_) => "Quitando origen…",
            }
            .into(),
        );
        self.marketplace_response = Some(receiver);
        std::thread::spawn(move || {
            let result = (|| -> std::io::Result<MarketplaceResult> {
                let updated_item = if let MarketplaceOperation::Update(id) = &operation {
                    Some(id.clone())
                } else {
                    None
                };
                let message = match operation {
                    MarketplaceOperation::Load => "Catálogo cargado".to_owned(),
                    MarketplaceOperation::CheckUpdates => "Comprobación terminada".to_owned(),
                    MarketplaceOperation::Refresh => {
                        let diagnostics = store.refresh_github_sources()?;
                        if diagnostics.is_empty() {
                            "Catálogo actualizado".into()
                        } else {
                            format!("Actualización parcial: {}", diagnostics.join(" · "))
                        }
                    }
                    MarketplaceOperation::Update(id) => {
                        store.update(&id)?;
                        "Skill actualizada".into()
                    }
                    MarketplaceOperation::AddLocal(path) => {
                        store.add_local_source(&path)?;
                        "Origen local agregado".into()
                    }
                    MarketplaceOperation::AddGithub(url) => {
                        store.add_github_source(&url)?;
                        "Repositorio agregado".into()
                    }
                    MarketplaceOperation::Install(id, scope) => {
                        store.install(&id, scope)?;
                        "Skill instalada".into()
                    }
                    MarketplaceOperation::InstallRemote(skill, scope) => {
                        store.install_remote(&skill, scope)?;
                        "Skill instalada".into()
                    }
                    MarketplaceOperation::SetEnabled(id, enabled) => {
                        store.set_enabled(&id, enabled)?;
                        if enabled {
                            "Skill activada"
                        } else {
                            "Skill desactivada"
                        }
                        .into()
                    }
                    MarketplaceOperation::Uninstall(id) => {
                        store.uninstall(&id)?;
                        "Skill desinstalada".into()
                    }
                    MarketplaceOperation::RemoveSource(id) => {
                        store.remove_source(&id)?;
                        "Origen eliminado".into()
                    }
                };
                let (updates, update_error) = if check_updates {
                    match store.check_updates() {
                        Ok(updates) => (Some(updates), None),
                        Err(error) => (None, Some(error.to_string())),
                    }
                } else {
                    (None, None)
                };
                let snapshot = store.snapshot()?;
                let mut provenance = BTreeMap::new();
                for item in &snapshot.items {
                    if item.installed_scope.is_some() {
                        if let Some(origin) = store.installed_provenance(&item.id)? {
                            provenance.insert(item.id.clone(), origin);
                        }
                    }
                }
                Ok(MarketplaceResult {
                    snapshot,
                    message,
                    provenance,
                    updates,
                    update_error,
                    updated_item,
                    checked_updates: check_updates,
                })
            })()
            .map_err(|error| error.to_string());
            let _ = sender.send((workspace, result));
        });
    }

    pub(super) fn poll_marketplace(&mut self) -> bool {
        let changed = self.poll_marketplace_operation();
        self.poll_marketplace_search() || changed
    }

    fn poll_marketplace_operation(&mut self) -> bool {
        let Some(receiver) = &self.marketplace_response else {
            return false;
        };
        let response = match receiver.try_recv() {
            Ok(response) => response,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.marketplace_response = None;
                let state = &mut self.view.marketplace;
                state.busy = false;
                state.update_checking = false;
                state.status_error = true;
                state.status = Some("No se pudo completar la operación".into());
                return true;
            }
        };
        self.marketplace_response = None;
        self.view.marketplace.busy = false;
        self.view.marketplace.update_checking = false;
        let (workspace, result) = response;
        if workspace != Path::new(&self.view.workspace) {
            self.marketplace_operation(MarketplaceOperation::Load);
            return true;
        }
        let state = &mut self.view.marketplace;
        match result {
            Ok(result) => {
                state.snapshot = result.snapshot;
                state.provenance = result.provenance;
                if let Some(updates) = result.updates {
                    state.updates = updates
                        .into_iter()
                        .map(|entry| (entry.item_id.clone(), entry))
                        .collect();
                    state.updates_checked = true;
                }
                if result.checked_updates {
                    state.update_error = result.update_error;
                }
                state.updates.retain(|id, _| {
                    state
                        .snapshot
                        .items
                        .iter()
                        .any(|item| item.id == *id && item.installed_scope.is_some())
                });
                if let Some(id) = result.updated_item {
                    state.updates.remove(&id);
                }
                state.status = Some(result.message);
                state.status_error = false;
                state.add_source_kind = None;
                state.source_draft.clear();
                state.add_menu_open = false;
                state.page = catalog_page(state).2;
            }
            Err(error) => {
                state.status = Some(error);
                state.status_error = true;
            }
        }
        true
    }

    fn request_marketplace_search(&mut self, force_refresh: bool) {
        let query = self.view.marketplace.explore.query.trim().to_owned();
        if self.view.marketplace.tab != MarketplaceTab::Explore || query.chars().count() < 2 {
            return;
        }
        self.marketplace_remote.invalidate();
        self.marketplace_remote.requested = Some((query, force_refresh));
        self.view.marketplace.explore.busy = true;
        self.view.marketplace.explore.error = None;
        self.view.marketplace.explore.page = None;
        self.view.marketplace.page = 0;
        self.start_marketplace_search();
    }

    fn start_marketplace_search(&mut self) {
        if self.marketplace_remote.response.is_some() {
            return;
        }
        let Some((query, force_refresh)) = self.marketplace_remote.requested.take() else {
            return;
        };
        let workspace = PathBuf::from(&self.view.workspace);
        let generation = self.marketplace_remote.generation;
        let store = MarketplaceStore::new(self.data_root.clone(), workspace.clone());
        let (sender, receiver) = mpsc::channel();
        self.marketplace_remote.response = Some(receiver);
        std::thread::spawn(move || {
            let result = store
                .search_remote(&query, force_refresh)
                .map_err(|error| error.to_string());
            let _ = sender.send((workspace, generation, query, result));
        });
    }

    fn poll_marketplace_search(&mut self) -> bool {
        let Some(receiver) = &self.marketplace_remote.response else {
            return false;
        };
        let response = match receiver.try_recv() {
            Ok(response) => Some(response),
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => None,
        };
        self.marketplace_remote.response = None;
        if let Some((workspace, generation, query, result)) = response {
            if workspace == Path::new(&self.view.workspace)
                && generation == self.marketplace_remote.generation
                && self.view.marketplace.tab == MarketplaceTab::Explore
                && query == self.view.marketplace.explore.query.trim()
            {
                let state = &mut self.view.marketplace.explore;
                state.busy = false;
                match result {
                    Ok(page) => {
                        state.page = Some(page);
                        state.error = None;
                    }
                    Err(error) => {
                        state.page = None;
                        state.error = Some(error);
                    }
                }
            }
        } else if self.marketplace_remote.requested.is_none() && self.view.marketplace.explore.busy
        {
            self.view.marketplace.explore.busy = false;
            self.view.marketplace.explore.error =
                Some("No se pudo completar la búsqueda. Vuelve a intentarlo.".into());
        }
        self.start_marketplace_search();
        true
    }

    pub(super) fn reset_marketplace_workspace(&mut self) {
        self.marketplace_remote.invalidate();
        self.view.marketplace = MarketplaceViewState::default();
        self.sync_marketplace_query_inputs();
    }

    fn sync_marketplace_query_inputs(&mut self) {
        let value = self.view.marketplace.active_query().to_owned();
        for id in ["marketplace_search", "ide_extensions_search"] {
            if self.tree.input_value(id) != Some(value.as_str()) {
                self.tree.set_input_value(id, &value);
            }
        }
    }

    pub(super) fn set_marketplace_query(&mut self, value: String) {
        self.view.marketplace.page = 0;
        if self.view.marketplace.tab == MarketplaceTab::Explore {
            if self.view.marketplace.explore.query != value {
                self.marketplace_remote.invalidate();
                self.view.marketplace.explore.page = None;
                self.view.marketplace.explore.busy = false;
                self.view.marketplace.explore.error = None;
                self.view.marketplace.explore.query = value;
            }
        } else {
            self.view.marketplace.query = value;
        }
        self.sync_marketplace_query_inputs();
    }

    pub(super) fn set_marketplace_tab(&mut self, tab: MarketplaceTab) {
        self.marketplace_remote.invalidate();
        let state = &mut self.view.marketplace;
        state.tab = tab;
        state.installed_only = tab == MarketplaceTab::Installed;
        state.page = 0;
        state.category_menu_open = false;
        state.explore.busy = false;
        state.selected_item_id = None;
        self.sync_marketplace_query_inputs();
    }

    pub(super) fn marketplace_source_url(&self, id: &str) -> Option<String> {
        if let Some(origin) = self.view.marketplace.provenance.get(id) {
            return source_url(&origin.repository, &origin.revision, &origin.directory);
        }
        if let Some(details) = crate::marketplace::starter_details(id) {
            return source_url(
                details.repository_url.strip_prefix("https://github.com/")?,
                &details.revision,
                &details.directory,
            );
        }
        self.view
            .marketplace
            .explore
            .page
            .as_ref()?
            .skills
            .iter()
            .find(|skill| skill.id == id)
            .map(|skill| format!("https://github.com/{}", skill.source))
    }
    pub(super) fn handle_marketplace_action(&mut self, id: &str) {
        if let Some(tab) = id
            .strip_prefix("marketplace_tab:")
            .or_else(|| id.strip_prefix("ide_marketplace_tab:"))
        {
            let tab = match tab {
                "recommended" => MarketplaceTab::Recommended,
                "explore" => MarketplaceTab::Explore,
                "installed" => MarketplaceTab::Installed,
                _ => return,
            };
            self.set_marketplace_tab(tab);
            return;
        }
        let state = &mut self.view.marketplace;
        match id {
            "marketplace_details_close" => state.selected_item_id = None,
            "marketplace_filter_all" => self.set_marketplace_tab(MarketplaceTab::Recommended),
            "marketplace_filter_installed" => self.set_marketplace_tab(MarketplaceTab::Installed),
            "marketplace_category_toggle" => state.category_menu_open = !state.category_menu_open,
            "marketplace_category_dismiss" => state.category_menu_open = false,
            "marketplace_page_prev" => state.page = state.page.saturating_sub(1),
            "marketplace_page_next" => {
                let (_, total, page) = catalog_page(state);
                state.page = (page + 1).min(total.saturating_sub(1) / PAGE_SIZE);
            }
            "marketplace_explore_search" => self.request_marketplace_search(false),
            "marketplace_explore_retry" => self.request_marketplace_search(true),
            "marketplace_check_updates" => {
                self.marketplace_operation(MarketplaceOperation::CheckUpdates)
            }
            "marketplace_scope_user" => state.install_scope = InstallScope::User,
            "marketplace_scope_workspace" => state.install_scope = InstallScope::Workspace,
            "marketplace_add_menu" | "marketplace_empty_add_menu" => {
                state.add_menu_open = !state.add_menu_open;
            }
            "marketplace_add_menu_dismiss" => state.add_menu_open = false,
            "marketplace_source_kind:local" => {
                state.add_source_kind = Some(SourceKind::Local);
                state.add_menu_open = false;
                state.source_draft.clear();
                state.status = None;
                state.status_error = false;
            }
            "marketplace_source_kind:github" => {
                state.add_source_kind = Some(SourceKind::Github);
                state.add_menu_open = false;
                state.source_draft.clear();
                state.status = None;
                state.status_error = false;
            }
            "marketplace_source_cancel" => {
                if !state.busy {
                    state.add_source_kind = None;
                    state.source_draft.clear();
                    state.status = None;
                    state.status_error = false;
                }
            }
            "marketplace_alert_cancel" => {
                state.pending_uninstall_id = None;
                state.pending_remove_source_id = None;
            }
            "marketplace_source_submit" => {
                if state.busy {
                    return;
                }
                let value = state.source_draft.trim().to_owned();
                if value.is_empty() {
                    state.status = Some("Indica la carpeta o URL del origen".to_owned());
                    state.status_error = true;
                    return;
                }
                let operation = match state.add_source_kind {
                    Some(SourceKind::Local) => MarketplaceOperation::AddLocal(PathBuf::from(value)),
                    Some(SourceKind::Github) => MarketplaceOperation::AddGithub(value),
                    None => return,
                };
                self.marketplace_operation(operation);
            }
            "marketplace_refresh" => {
                if state.tab == MarketplaceTab::Explore {
                    self.request_marketplace_search(true);
                } else {
                    self.marketplace_operation(MarketplaceOperation::Refresh);
                }
            }
            _ => {
                if let Some(category) = id.strip_prefix("marketplace_category:") {
                    state.category = (category != "all").then(|| category.to_owned());
                    state.category_menu_open = false;
                    state.page = 0;
                } else if let Some(item) = id.strip_prefix("marketplace_update:") {
                    if state.updates.contains_key(item) {
                        self.marketplace_operation(MarketplaceOperation::Update(item.to_owned()));
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_details:") {
                    if state.find_item(item).is_some() {
                        state.selected_item_id = Some(item.to_owned());
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_install:") {
                    let scope = state.install_scope;
                    if state
                        .find_item(item)
                        .is_none_or(|entry| entry.installed_scope.is_some())
                    {
                        return;
                    }
                    if item.starts_with("skills-sh:") {
                        if let Some(skill) = state
                            .explore
                            .page
                            .as_ref()
                            .and_then(|page| page.skills.iter().find(|skill| skill.id == item))
                            .cloned()
                        {
                            self.marketplace_operation(MarketplaceOperation::InstallRemote(
                                skill, scope,
                            ));
                        }
                    } else {
                        self.marketplace_operation(MarketplaceOperation::Install(
                            item.to_owned(),
                            scope,
                        ));
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_toggle:") {
                    if let Some(enabled) = state
                        .snapshot
                        .items
                        .iter()
                        .find(|entry| entry.id == item)
                        .map(|entry| !entry.enabled)
                    {
                        self.marketplace_operation(MarketplaceOperation::SetEnabled(
                            item.to_owned(),
                            enabled,
                        ));
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_uninstall:") {
                    if !state.busy
                        && state
                            .snapshot
                            .items
                            .iter()
                            .any(|entry| entry.id == item && entry.installed_scope.is_some())
                    {
                        state.pending_uninstall_id = Some(item.to_owned());
                    }
                } else if let Some(source) = id.strip_prefix("marketplace_remove_source:") {
                    if !state.busy
                        && state
                            .snapshot
                            .sources
                            .iter()
                            .any(|entry| entry.id == source)
                    {
                        state.pending_remove_source_id = Some(source.to_owned());
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_uninstall_confirm:") {
                    if state.pending_uninstall_id.as_deref() == Some(item) && !state.busy {
                        state.pending_uninstall_id = None;
                        self.marketplace_operation(MarketplaceOperation::Uninstall(
                            item.to_owned(),
                        ));
                    }
                } else if let Some(source) = id.strip_prefix("marketplace_remove_source_confirm:") {
                    if state.pending_remove_source_id.as_deref() == Some(source) && !state.busy {
                        state.pending_remove_source_id = None;
                        self.marketplace_operation(MarketplaceOperation::RemoveSource(
                            source.to_owned(),
                        ));
                    }
                }
            }
        }
    }
}

fn source_url(repository: &str, revision: &str, directory: &str) -> Option<String> {
    let mut url = reqwest::Url::parse("https://github.com/").ok()?;
    {
        let mut path = url.path_segments_mut().ok()?;
        path.pop_if_empty()
            .extend(repository.split('/'))
            .push("tree")
            .push(revision);
        if !directory.is_empty() {
            path.extend(directory.split('/'));
        }
    }
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = root.path().join("workspace").display().to_string();
        std::fs::create_dir_all(&app.view.workspace).unwrap();
        (root, app)
    }

    #[test]
    fn marketplace_tabs_preserve_separate_queries_and_input_controllers() {
        let (_root, mut app) = fixture();
        app.tree = UiTree::new(Element::column(vec![
            Element::text_input("marketplace_search", "", ""),
            Element::text_input("ide_extensions_search", "", ""),
        ]));
        app.tree.layout(Size::new(600.0, 200.0));
        app.set_marketplace_query("Rust".into());
        app.handle_marketplace_action("ide_marketplace_tab:explore");
        assert_eq!(app.tree.input_value("marketplace_search"), Some(""));
        app.set_marketplace_query("react".into());
        assert!(
            !app.marketplace_remote.pending(),
            "typing must not issue a request"
        );
        app.handle_marketplace_action("marketplace_tab:installed");
        assert_eq!(app.tree.input_value("ide_extensions_search"), Some("Rust"));
        app.handle_marketplace_action("marketplace_tab:explore");
        assert_eq!(app.tree.input_value("marketplace_search"), Some("react"));
        assert_eq!(app.view.marketplace.page, 0);
    }

    #[test]
    fn marketplace_stale_search_cannot_replace_a_new_query_or_workspace() {
        let (_root, mut app) = fixture();
        app.set_marketplace_tab(MarketplaceTab::Explore);
        app.set_marketplace_query("react".into());
        let generation = app.marketplace_remote.generation;
        let (tx, rx) = mpsc::channel();
        app.marketplace_remote.response = Some(rx);
        app.set_marketplace_query("rust".into());
        tx.send((
            PathBuf::from(&app.view.workspace),
            generation,
            "react".into(),
            Ok(RemoteSearchPage {
                query: "react".into(),
                skills: vec![],
                from_cache: false,
                stale: false,
                warning: None,
            }),
        ))
        .unwrap();
        assert!(app.poll_marketplace_search());
        assert!(app.view.marketplace.explore.page.is_none());
        let (tx, rx) = mpsc::channel();
        app.marketplace_remote.response = Some(rx);
        tx.send((
            PathBuf::from("/different-project"),
            app.marketplace_remote.generation,
            "rust".into(),
            Err("old failure".into()),
        ))
        .unwrap();
        app.poll_marketplace_search();
        assert!(app.view.marketplace.explore.error.is_none());
    }

    #[test]
    fn marketplace_same_commit_manifest_update_remains_actionable() {
        let (_root, mut app) = fixture();
        let id = "starter-anthropic:frontend-design";
        let mut snapshot =
            MarketplaceStore::new(app.data_root.clone(), PathBuf::from(&app.view.workspace))
                .snapshot()
                .unwrap();
        snapshot
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .unwrap()
            .installed_scope = Some(InstallScope::User);
        let revision = "a".repeat(40);
        let provenance = BTreeMap::from([(
            id.into(),
            SkillProvenance {
                repository: "anthropics/skills".into(),
                revision: revision.clone(),
                directory: "skills/frontend-design".into(),
                manifest_digest: None,
            },
        )]);
        let (tx, rx) = mpsc::channel();
        app.marketplace_response = Some(rx);
        tx.send((
            PathBuf::from(&app.view.workspace),
            Ok(MarketplaceResult {
                snapshot,
                message: "checked".into(),
                provenance,
                updates: Some(vec![SkillUpdate {
                    item_id: id.into(),
                    installed_revision: Some(revision.clone()),
                    available_revision: revision,
                }]),
                update_error: None,
                updated_item: None,
                checked_updates: true,
            }),
        ))
        .unwrap();
        app.poll_marketplace_operation();
        assert!(app.view.marketplace.updates.contains_key(id));
        assert!(app.view.marketplace.updates_checked);
    }
    #[test]
    fn marketplace_project_switch_resets_discovery_and_update_state_outside_ide() {
        let (root, mut app) = fixture();
        app.view.marketplace.tab = MarketplaceTab::Explore;
        app.view.marketplace.explore.query = "old workspace".into();
        app.view.marketplace.updates_checked = true;
        app.view.marketplace.update_error = Some("old failure".into());
        let next = root.path().join("next-project");
        std::fs::create_dir(&next).unwrap();
        app.change_workspace(next.display().to_string());
        assert_eq!(app.view.marketplace.tab, MarketplaceTab::Recommended);
        assert!(app.view.marketplace.explore.query.is_empty());
        assert!(app.view.marketplace.update_error.is_none());
        assert!(!app.view.marketplace.updates_checked);
    }

    #[test]
    fn marketplace_provenance_links_encode_directory_segments() {
        let url = source_url(
            "owner/repository",
            &"a".repeat(40),
            "skills/with space#hash",
        )
        .unwrap();
        assert!(url.ends_with("/skills/with%20space%23hash"));
        assert!(reqwest::Url::parse(&url).unwrap().fragment().is_none());
    }

    #[test]
    fn marketplace_refresh_in_explore_queues_a_search_without_mutating_sources() {
        let (_root, mut app) = fixture();
        app.set_marketplace_tab(MarketplaceTab::Explore);
        app.set_marketplace_query("react".into());
        // An outstanding worker makes the next confirmed query queue; no HTTP.
        let (_sender, receiver) = mpsc::channel();
        app.marketplace_remote.response = Some(receiver);
        app.handle_marketplace_action("marketplace_refresh");
        assert_eq!(
            app.marketplace_remote.requested,
            Some(("react".into(), true))
        );
        assert!(app.view.marketplace.explore.busy);
        assert!(app.marketplace_response.is_none());
        assert!(!app.view.marketplace.busy);
    }
}
