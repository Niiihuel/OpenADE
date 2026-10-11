//! Host-owned workers keep discovery, downloads and update checks off the UI thread.
use super::*;
use crate::marketplace::{
    MarketplaceSnapshot, RemoteSearchPage, RemoteSkill, SkillProvenance, SkillUpdate,
};
use crate::marketplace_view::{MarketplaceFilter, MarketplaceReadme, GROUP_STEP};
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
type ReadmeResponse = (PathBuf, String, Result<Option<String>, String>);

#[derive(Default)]
pub(super) struct MarketplaceRemoteHost {
    response: Option<Receiver<SearchResponse>>,
    requested: Option<(String, bool)>,
    generation: u64,
    readme: Option<Receiver<ReadmeResponse>>,
}

impl MarketplaceRemoteHost {
    pub(super) fn pending(&self) -> bool {
        self.response.is_some() || self.requested.is_some() || self.readme.is_some()
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
        self.view.marketplace.menu_item = None;
        self.view.marketplace.busy_item = match &operation {
            MarketplaceOperation::Update(id)
            | MarketplaceOperation::Install(id, _)
            | MarketplaceOperation::SetEnabled(id, _)
            | MarketplaceOperation::Uninstall(id) => Some(id.clone()),
            MarketplaceOperation::InstallRemote(skill, _) => Some(skill.id.clone()),
            _ => None,
        };
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
        let changed = self.poll_marketplace_search() || changed;
        self.sync_marketplace_readme() || changed
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
                state.busy_item = None;
                state.update_checking = false;
                state.status_error = true;
                state.status = Some("No se pudo completar la operación".into());
                return true;
            }
        };
        self.marketplace_response = None;
        self.view.marketplace.busy = false;
        self.view.marketplace.busy_item = None;
        self.view.marketplace.update_checking = false;
        // An install, update or removal changes what is on disk to read.
        self.view.marketplace.readme = None;
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
                if state
                    .selected_item_id
                    .as_deref()
                    .is_some_and(|id| state.find_item(id).is_none())
                {
                    state.selected_item_id = None;
                }
            }
            Err(error) => {
                state.status = Some(error);
                state.status_error = true;
            }
        }
        true
    }

    fn request_marketplace_search(&mut self, force_refresh: bool) {
        let query = self.view.marketplace.query.trim().to_owned();
        if query.chars().count() < 2 {
            return;
        }
        self.marketplace_remote.invalidate();
        self.marketplace_remote.requested = Some((query.clone(), force_refresh));
        let explore = &mut self.view.marketplace.explore;
        explore.query = query;
        explore.busy = true;
        explore.error = None;
        explore.page = None;
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
                && query == self.view.marketplace.explore.query
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

    /// Read the selected item's SKILL.md on a worker, once per selection and
    /// again after an operation changed what is installed.
    fn sync_marketplace_readme(&mut self) -> bool {
        let mut changed = false;
        if let Some(receiver) = &self.marketplace_remote.readme {
            let (workspace, id, body) = match receiver.try_recv() {
                Ok(response) => response,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => (
                    PathBuf::from(&self.view.workspace),
                    String::new(),
                    Err("No se pudo leer SKILL.md".to_owned()),
                ),
            };
            self.marketplace_remote.readme = None;
            let current = workspace == Path::new(&self.view.workspace);
            if let Some(readme) = self
                .view
                .marketplace
                .readme
                .as_mut()
                .filter(|readme| current && readme.body.is_none())
                .filter(|readme| id.is_empty() || readme.item_id == id)
            {
                readme.body = Some(body);
                changed = true;
            }
        }
        let state = &mut self.view.marketplace;
        let Some(id) = state.selected_item_id.clone() else {
            return state.readme.take().is_some() || changed;
        };
        if state
            .readme
            .as_ref()
            .is_some_and(|readme| readme.item_id == id && readme.body.is_some())
        {
            return changed;
        }
        let waiting = state
            .readme
            .as_ref()
            .is_some_and(|readme| readme.item_id == id);
        state.readme = Some(MarketplaceReadme {
            item_id: id.clone(),
            body: None,
        });
        if self.marketplace_remote.readme.is_some() {
            // The reader in course answers for another item; ask after it.
            return changed || !waiting;
        }
        let workspace = PathBuf::from(&self.view.workspace);
        let store = MarketplaceStore::new(self.data_root.clone(), workspace.clone());
        let (sender, receiver) = mpsc::channel();
        self.marketplace_remote.readme = Some(receiver);
        std::thread::spawn(move || {
            let body = store.readme(&id).map_err(|error| error.to_string());
            let _ = sender.send((workspace, id, body));
        });
        true
    }

    pub(super) fn reset_marketplace_workspace(&mut self) {
        self.marketplace_remote.invalidate();
        self.marketplace_remote.readme = None;
        self.view.marketplace = MarketplaceViewState::default();
        self.sync_marketplace_query_inputs();
    }

    fn sync_marketplace_query_inputs(&mut self) {
        let value = self.view.marketplace.query.clone();
        for id in ["marketplace_search", "ide_extensions_search"] {
            if self.tree.input_value(id) != Some(value.as_str()) {
                self.tree.set_input_value(id, &value);
            }
        }
    }

    /// Typing filters the local catalog at once. A skills.sh search is asked
    /// for with Enter, and its results leave when the text stops matching it.
    pub(super) fn set_marketplace_query(&mut self, value: String) {
        let state = &mut self.view.marketplace;
        state.visible = GROUP_STEP;
        if state.explore.query != value.trim() {
            self.marketplace_remote.invalidate();
            state.explore = Default::default();
        }
        state.query = value;
        self.sync_marketplace_query_inputs();
    }

    pub(super) fn set_marketplace_filter(&mut self, filter: MarketplaceFilter) {
        let state = &mut self.view.marketplace;
        state.filter = filter;
        state.filter_menu_open = false;
        state.visible = GROUP_STEP;
    }

    fn install_marketplace_item(&mut self, item: &str, scope: InstallScope) {
        let state = &mut self.view.marketplace;
        if state
            .find_item(item)
            .is_none_or(|entry| entry.installed_scope.is_some())
        {
            return;
        }
        state.install_scope = scope;
        if item.starts_with("skills-sh:") {
            if let Some(skill) = state
                .explore
                .page
                .as_ref()
                .and_then(|page| page.skills.iter().find(|skill| skill.id == item))
                .cloned()
            {
                self.marketplace_operation(MarketplaceOperation::InstallRemote(skill, scope));
            }
        } else {
            self.marketplace_operation(MarketplaceOperation::Install(item.to_owned(), scope));
        }
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
        // Any action other than the menu's own toggle closes the open menu.
        let toggled = id
            .strip_prefix("marketplace_row_menu:")
            .map(|item| format!("row:{item}"))
            .or_else(|| {
                id.strip_prefix("marketplace_detail_menu:")
                    .map(|item| format!("detail:{item}"))
            });
        let state = &mut self.view.marketplace;
        let was_open = state.menu_item.take();
        if let Some(key) = toggled {
            if was_open.as_deref() != Some(key.as_str()) && !state.busy {
                state.menu_item = Some(key);
            }
            return;
        }
        if let Some(filter) = id
            .strip_prefix("marketplace_filter:")
            .and_then(MarketplaceFilter::from_key)
        {
            self.set_marketplace_filter(filter);
            return;
        }
        match id {
            "marketplace_menu_dismiss" => {}
            "marketplace_details_close" => state.selected_item_id = None,
            "marketplace_filter_toggle" => state.filter_menu_open = !state.filter_menu_open,
            "marketplace_filter_dismiss" => state.filter_menu_open = false,
            "marketplace_filter_clear" => {
                state.category = None;
                self.set_marketplace_filter(MarketplaceFilter::All);
            }
            "marketplace_more" => state.visible += GROUP_STEP,
            "marketplace_explore_search" => self.request_marketplace_search(false),
            "marketplace_explore_retry" => self.request_marketplace_search(true),
            "marketplace_check_updates" => {
                self.marketplace_operation(MarketplaceOperation::CheckUpdates)
            }
            "marketplace_reload" => self.marketplace_operation(MarketplaceOperation::Load),
            "marketplace_add_menu" => {
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
            "marketplace_refresh" => self.marketplace_operation(MarketplaceOperation::Refresh),
            _ => {
                if let Some(category) = id.strip_prefix("marketplace_category:") {
                    state.category = (category != "all").then(|| category.to_owned());
                    state.filter_menu_open = false;
                    state.visible = GROUP_STEP;
                } else if let Some(item) = id.strip_prefix("marketplace_update:") {
                    if state.updates.contains_key(item) {
                        self.marketplace_operation(MarketplaceOperation::Update(item.to_owned()));
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_details:") {
                    if state.find_item(item).is_some() {
                        state.selected_item_id = Some(item.to_owned());
                    }
                } else if let Some(item) = id.strip_prefix("marketplace_install_user:") {
                    self.install_marketplace_item(item, InstallScope::User);
                } else if let Some(item) = id.strip_prefix("marketplace_install_workspace:") {
                    self.install_marketplace_item(item, InstallScope::Workspace);
                } else if let Some(item) = id.strip_prefix("marketplace_install:") {
                    let scope = state.install_scope;
                    self.install_marketplace_item(item, scope);
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

    /// A local source with one skill, loaded into the view without a worker.
    fn local_fixture() -> (tempfile::TempDir, AgentsWindow, String) {
        let (root, mut app) = fixture();
        let skill = root.path().join("catalog/skills/git-review");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: git-review\ndescription: Review changes\nlicense: MIT\n---\n\n## Steps\nRead the diff.\n",
        )
        .unwrap();
        let store =
            MarketplaceStore::new(app.data_root.clone(), PathBuf::from(&app.view.workspace));
        let source = store
            .add_local_source(&root.path().join("catalog"))
            .unwrap();
        app.view.marketplace.snapshot = store.snapshot().unwrap();
        (root, app, format!("{source}:git-review"))
    }

    /// Let the worker of a local, offline operation finish.
    fn settle(app: &mut AgentsWindow) {
        for _ in 0..400 {
            app.poll_marketplace();
            if app.marketplace_response.is_none() && !app.marketplace_remote.pending() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the marketplace worker did not finish");
    }

    #[test]
    fn one_query_feeds_both_inputs_and_only_enter_searches_online() {
        let (_root, mut app) = fixture();
        app.tree = UiTree::new(Element::column(vec![
            Element::text_input("marketplace_search", "", ""),
            Element::text_input("ide_extensions_search", "", ""),
        ]));
        app.tree.layout(Size::new(600.0, 200.0));
        app.view.marketplace.visible = 90;
        app.set_marketplace_query("react".into());
        assert_eq!(app.tree.input_value("marketplace_search"), Some("react"));
        assert_eq!(app.tree.input_value("ide_extensions_search"), Some("react"));
        assert_eq!(app.view.marketplace.visible, GROUP_STEP);
        assert!(
            !app.marketplace_remote.pending(),
            "typing must not issue a request"
        );
        // A filter is independent of the text.
        app.handle_marketplace_action("marketplace_filter:installed");
        assert_eq!(app.view.marketplace.filter, MarketplaceFilter::Installed);
        assert_eq!(app.view.marketplace.query, "react");
        app.view.marketplace.category = Some("Datos".into());
        app.handle_marketplace_action("marketplace_filter_clear");
        assert_eq!(app.view.marketplace.filter, MarketplaceFilter::All);
        assert!(app.view.marketplace.category.is_none());

        // An outstanding worker makes the confirmed query queue; no HTTP.
        let (_sender, receiver) = mpsc::channel();
        app.marketplace_remote.response = Some(receiver);
        app.handle_marketplace_action("marketplace_explore_search");
        assert_eq!(
            app.marketplace_remote.requested,
            Some(("react".into(), false))
        );
        assert!(app.view.marketplace.explore.busy);
        assert_eq!(app.view.marketplace.explore.query, "react");
        app.handle_marketplace_action("marketplace_explore_retry");
        assert_eq!(
            app.marketplace_remote.requested,
            Some(("react".into(), true))
        );
        assert!(app.marketplace_response.is_none());
        assert!(!app.view.marketplace.busy);
        // Editing the text drops the search that no longer matches it.
        app.set_marketplace_query("rea".into());
        assert!(!app.view.marketplace.explore.busy);
        assert!(app.marketplace_remote.requested.is_none());
        // One character is not a search.
        app.set_marketplace_query("r".into());
        app.handle_marketplace_action("marketplace_explore_search");
        assert!(app.marketplace_remote.requested.is_none());
    }

    #[test]
    fn marketplace_stale_search_cannot_replace_a_new_query_or_workspace() {
        let (_root, mut app) = fixture();
        app.set_marketplace_query("react".into());
        let generation = app.marketplace_remote.generation;
        let (tx, rx) = mpsc::channel();
        app.marketplace_remote.response = Some(rx);
        app.view.marketplace.explore.query = "react".into();
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
        app.view.marketplace.explore.query = "rust".into();
        tx.send((
            PathBuf::from("/different-project"),
            app.marketplace_remote.generation,
            "rust".into(),
            Err("old failure".into()),
        ))
        .unwrap();
        app.poll_marketplace_search();
        assert!(app.view.marketplace.explore.error.is_none());
        // The answer to the current query of the current project is kept.
        let (tx, rx) = mpsc::channel();
        app.marketplace_remote.response = Some(rx);
        tx.send((
            PathBuf::from(&app.view.workspace),
            app.marketplace_remote.generation,
            "rust".into(),
            Err("sin conexión".into()),
        ))
        .unwrap();
        app.poll_marketplace_search();
        assert_eq!(
            app.view.marketplace.explore.error.as_deref(),
            Some("sin conexión")
        );
    }

    #[test]
    fn selecting_an_item_reads_its_instructions_off_the_ui_thread() {
        let (_root, mut app, id) = local_fixture();
        app.handle_marketplace_action("marketplace_details:missing");
        assert!(app.view.marketplace.selected_item_id.is_none());
        app.handle_marketplace_action(&format!("marketplace_details:{id}"));
        assert_eq!(app.view.marketplace.selected_item_id.as_deref(), Some(id.as_str()));
        assert!(app.poll_marketplace());
        assert_eq!(
            app.view.marketplace.readme,
            Some(MarketplaceReadme {
                item_id: id.clone(),
                body: None
            }),
            "the first frame shows a loading state, not a read on the UI thread"
        );
        settle(&mut app);
        assert_eq!(
            app.view.marketplace.readme.as_ref().unwrap().body,
            Some(Ok(Some("## Steps\nRead the diff.".into())))
        );
        // A curated entry is not downloaded to be read.
        let starter = "starter-anthropic:frontend-design";
        app.view.marketplace.snapshot.items.push(
            MarketplaceStore::new(app.data_root.clone(), PathBuf::from(&app.view.workspace))
                .snapshot()
                .unwrap()
                .items
                .into_iter()
                .find(|item| item.id == starter)
                .unwrap(),
        );
        app.handle_marketplace_action(&format!("marketplace_details:{starter}"));
        settle(&mut app);
        assert_eq!(
            app.view.marketplace.readme,
            Some(MarketplaceReadme {
                item_id: starter.into(),
                body: Some(Ok(None))
            })
        );
        app.handle_marketplace_action("marketplace_details_close");
        assert!(app.view.marketplace.selected_item_id.is_none());
        app.poll_marketplace();
        assert!(app.view.marketplace.readme.is_none());
    }

    #[test]
    fn an_action_menu_opens_one_at_a_time_and_any_action_closes_it() {
        let (_root, mut app, id) = local_fixture();
        let state = |app: &AgentsWindow| app.view.marketplace.menu_item.clone();
        app.handle_marketplace_action(&format!("marketplace_row_menu:{id}"));
        assert_eq!(state(&app), Some(format!("row:{id}")));
        app.handle_marketplace_action(&format!("marketplace_detail_menu:{id}"));
        assert_eq!(state(&app), Some(format!("detail:{id}")));
        app.handle_marketplace_action(&format!("marketplace_detail_menu:{id}"));
        assert_eq!(state(&app), None);
        app.handle_marketplace_action(&format!("marketplace_row_menu:{id}"));
        app.handle_marketplace_action("marketplace_menu_dismiss");
        assert_eq!(state(&app), None);
        app.handle_marketplace_action(&format!("marketplace_row_menu:{id}"));
        app.handle_marketplace_action("marketplace_filter_toggle");
        assert_eq!(state(&app), None);
        assert!(app.view.marketplace.filter_menu_open);
        // A running operation opens no menu.
        app.view.marketplace.busy = true;
        app.handle_marketplace_action(&format!("marketplace_row_menu:{id}"));
        assert_eq!(state(&app), None);
    }

    #[test]
    fn installing_names_its_scope_and_runs_the_same_store_operations() {
        let (_root, mut app, id) = local_fixture();
        let scope = |app: &AgentsWindow| {
            app.view
                .marketplace
                .find_item(&id)
                .unwrap()
                .installed_scope
        };
        // The menu's choice installs to that scope and becomes the default.
        app.handle_marketplace_action(&format!("marketplace_row_menu:{id}"));
        app.handle_marketplace_action(&format!("marketplace_install_workspace:{id}"));
        assert!(app.view.marketplace.busy);
        assert_eq!(app.view.marketplace.busy_item.as_deref(), Some(id.as_str()));
        assert!(app.view.marketplace.menu_item.is_none());
        settle(&mut app);
        assert!(!app.view.marketplace.busy);
        assert!(app.view.marketplace.busy_item.is_none());
        assert_eq!(scope(&app), Some(InstallScope::Workspace));
        assert_eq!(app.view.marketplace.install_scope, InstallScope::Workspace);
        assert_eq!(app.view.marketplace.status.as_deref(), Some("Skill instalada"));
        // Installing again is ignored; nothing is queued.
        app.handle_marketplace_action(&format!("marketplace_install:{id}"));
        assert!(!app.view.marketplace.busy);

        app.handle_marketplace_action(&format!("marketplace_toggle:{id}"));
        settle(&mut app);
        assert!(!app.view.marketplace.find_item(&id).unwrap().enabled);

        // Removal still waits for its confirmation.
        app.handle_marketplace_action(&format!("marketplace_uninstall:{id}"));
        assert!(!app.view.marketplace.busy);
        assert_eq!(
            app.view.marketplace.pending_uninstall_id.as_deref(),
            Some(id.as_str())
        );
        app.handle_marketplace_action(&format!("marketplace_uninstall_confirm:{id}"));
        settle(&mut app);
        assert_eq!(scope(&app), None);

        // The detail's primary button installs to the remembered scope.
        app.view.marketplace.install_scope = InstallScope::User;
        app.handle_marketplace_action(&format!("marketplace_install:{id}"));
        settle(&mut app);
        assert_eq!(scope(&app), Some(InstallScope::User));
    }

    #[test]
    fn the_instructions_are_read_again_after_an_operation_changes_the_item() {
        let (_root, mut app, id) = local_fixture();
        app.handle_marketplace_action(&format!("marketplace_details:{id}"));
        settle(&mut app);
        assert!(app.view.marketplace.readme.as_ref().unwrap().body.is_some());
        app.handle_marketplace_action(&format!("marketplace_install_user:{id}"));
        settle(&mut app);
        assert_eq!(
            app.view.marketplace.readme.as_ref().unwrap().body,
            Some(Ok(Some("## Steps\nRead the diff.".into())))
        );
        assert_eq!(app.view.marketplace.selected_item_id.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn show_more_grows_the_list_and_a_new_filter_starts_it_over() {
        let (_root, mut app) = fixture();
        app.handle_marketplace_action("marketplace_more");
        assert_eq!(app.view.marketplace.visible, 2 * GROUP_STEP);
        app.handle_marketplace_action("marketplace_category:Datos");
        assert_eq!(app.view.marketplace.category.as_deref(), Some("Datos"));
        assert_eq!(app.view.marketplace.visible, GROUP_STEP);
        app.handle_marketplace_action("marketplace_category:all");
        assert!(app.view.marketplace.category.is_none());
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
        app.view.marketplace.filter = MarketplaceFilter::Installed;
        app.view.marketplace.selected_item_id = Some("old:item".into());
        app.view.marketplace.explore.query = "old workspace".into();
        app.view.marketplace.updates_checked = true;
        app.view.marketplace.update_error = Some("old failure".into());
        let next = root.path().join("next-project");
        std::fs::create_dir(&next).unwrap();
        app.change_workspace(next.display().to_string());
        assert_eq!(app.view.marketplace.filter, MarketplaceFilter::All);
        assert!(app.view.marketplace.selected_item_id.is_none());
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
}
