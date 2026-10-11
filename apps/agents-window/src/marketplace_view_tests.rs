use super::*;
use crate::marketplace::{MarketplaceSource, MarketplaceStore, RemoteSkill};
use openide_ui::{DrawCommand, Frame, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};

const FRONTEND: &str = "starter-anthropic:frontend-design";
const REMOTE: &str = "skills-sh:example/skills:quality";

fn layout(root: Element, width: f32, height: f32) -> Frame {
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, height))
}

fn has(frame: &Frame, id: &str) -> bool {
    frame.regions.iter().any(|region| region.id == id)
}

fn says(frame: &Frame, needle: &str) -> bool {
    frame
        .text_layouts()
        .iter()
        .any(|block| block.source.contains(needle))
        || frame.commands.iter().any(
            |command| matches!(command, DrawCommand::Text { text, .. } if text.contains(needle)),
        )
}

fn rect(frame: &Frame, id: &str) -> openide_ui::Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
        .rect
}

fn skill(id: &str, name: &str, installed: Option<InstallScope>) -> MarketplaceItem {
    MarketplaceItem {
        id: id.into(),
        name: name.into(),
        description: "Review code changes".into(),
        license: Some("MIT".into()),
        source_name: "team-skills".into(),
        source_kind: SourceKind::Local,
        installed_scope: installed,
        enabled: installed.is_some(),
        source_available: true,
    }
}

/// One local source with one skill that is not installed.
fn sample_state() -> MarketplaceViewState {
    MarketplaceViewState {
        snapshot: MarketplaceSnapshot {
            sources: vec![MarketplaceSource {
                id: "source-1".into(),
                name: "team-skills".into(),
                location: "C:/team-skills".into(),
                kind: SourceKind::Local,
            }],
            items: vec![skill("source-1:git-review", "git-review", None)],
            diagnostics: Vec::new(),
        },
        ..Default::default()
    }
}

/// The bundled catalog, read without the network or the user's folders.
fn starter_state() -> MarketplaceViewState {
    let root = tempfile::tempdir().unwrap();
    MarketplaceViewState {
        snapshot: MarketplaceStore::new(root.path().join("data"), root.path().into())
            .snapshot()
            .unwrap(),
        ..Default::default()
    }
}

fn remote_page() -> RemoteSearchPage {
    RemoteSearchPage {
        query: "quality".into(),
        skills: vec![RemoteSkill {
            id: REMOTE.into(),
            name: "quality".into(),
            source: "example/skills".into(),
            installs: 23,
        }],
        from_cache: false,
        stale: false,
        warning: None,
    }
}

fn remote_state() -> MarketplaceViewState {
    MarketplaceViewState {
        query: "quality".into(),
        explore: MarketplaceExploreState {
            query: "quality".into(),
            page: Some(remote_page()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn list(state: &MarketplaceViewState, width: f32) -> Element {
    list_sized(state, width, 700.0)
}

fn list_sized(state: &MarketplaceViewState, width: f32, height: f32) -> Element {
    Element::column(catalog_list(
        state,
        width,
        "marketplace_search",
        "marketplace_details:",
        theme::BACKGROUND,
        None,
    ))
    .size(Some(width), Some(height))
}

fn group_sizes(state: &MarketplaceViewState) -> Vec<(&'static str, usize)> {
    catalog_groups(state)
        .iter()
        .map(|group| (group.key, group.items.len()))
        .collect()
}

#[test]
fn the_catalog_is_grouped_by_what_is_installed_and_where_the_rest_comes_from() {
    let mut state = starter_state();
    let starters = state.snapshot.items.len();
    assert_eq!(group_sizes(&state), [("recommended", starters)]);

    state.snapshot.items[0].installed_scope = Some(InstallScope::User);
    state
        .snapshot
        .items
        .push(skill("source-1:git-review", "git-review", None));
    state.explore.page = Some(remote_page());
    assert_eq!(
        group_sizes(&state),
        [
            ("installed", 1),
            ("recommended", starters - 1),
            ("sources", 1),
            ("remote", 1)
        ]
    );

    // A result that is installed is listed once, as installed.
    let mut installed = remote_page().skills[0].item();
    installed.installed_scope = Some(InstallScope::Workspace);
    state.snapshot.items.push(installed);
    assert!(!group_sizes(&state).iter().any(|(key, _)| *key == "remote"));
    assert_eq!(group_sizes(&state)[0], ("installed", 2));
}

#[test]
fn filters_narrow_the_groups_by_state_category_and_text() {
    let mut state = starter_state();
    let starters = state.snapshot.items.len();
    let id = state.snapshot.items[0].id.clone();
    state.snapshot.items[0].installed_scope = Some(InstallScope::User);
    state.explore.page = Some(remote_page());

    state.filter = MarketplaceFilter::Installed;
    assert_eq!(group_sizes(&state), [("installed", 1)]);
    state.filter = MarketplaceFilter::Updates;
    assert!(group_sizes(&state).is_empty());
    state.updates.insert(
        id.clone(),
        SkillUpdate {
            item_id: id,
            installed_revision: None,
            available_revision: "b".repeat(40),
        },
    );
    assert_eq!(group_sizes(&state), [("installed", 1)]);
    state.filter = MarketplaceFilter::Available;
    assert_eq!(
        group_sizes(&state),
        [("recommended", starters - 1), ("remote", 1)]
    );

    // A category is catalog metadata: it leaves out skills.sh and local sources.
    state.filter = MarketplaceFilter::All;
    state.category = Some("Documentación".into());
    let groups = catalog_groups(&state);
    assert!(!groups.is_empty());
    assert!(groups.iter().flat_map(|group| &group.items).all(|item| {
        crate::marketplace::starter_category(&item.id) == Some("Documentación")
    }));

    state.category = None;
    state.explore.page = None;
    state.query = "  FRONTEND-design ".into();
    let groups = catalog_groups(&state);
    assert_eq!(groups.iter().map(|group| group.items.len()).sum::<usize>(), 1);
    state.query = "anthropic".into();
    assert!(catalog_groups(&state)
        .iter()
        .flat_map(|group| &group.items)
        .all(|item| item.source_name.contains("Anthropic")));
}

#[test]
fn a_group_builds_one_step_of_rows_and_offers_the_rest() {
    let mut state = starter_state();
    let starters = state.snapshot.items.len();
    assert!(starters > GROUP_STEP);
    let rows = |frame: &Frame| {
        frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("marketplace_row_menu:"))
            .count()
    };
    // A scroll area reports no regions for rows out of view: lay it out tall.
    let tall = |state: &MarketplaceViewState| layout(list_sized(state, 320.0, 3000.0), 320.0, 3000.0);
    let frame = tall(&state);
    assert_eq!(rows(&frame), GROUP_STEP);
    assert!(says(
        &frame,
        &format!("Mostrar más ({} restantes)", starters - GROUP_STEP)
    ));
    // The header counts every match, not only the rows built.
    assert!(says(&frame, &starters.to_string()));
    assert!(!frame.regions.iter().any(|region| region.id.contains("page")));

    state.visible += GROUP_STEP;
    let frame = tall(&state);
    assert_eq!(rows(&frame), starters);
    assert!(!says(&frame, "Mostrar más"));
}

#[test]
fn a_row_says_what_the_item_is_and_keeps_its_action_in_place() {
    let mut state = sample_state();
    state
        .snapshot
        .items
        .push(skill("source-1:installed", "installed-skill", Some(InstallScope::Workspace)));
    let mut tree = UiTree::new(list(&state, 300.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(300.0, 700.0));
    for needle in ["git-review", "Review code changes", "team-skills · Carpeta local", "Proyecto"] {
        assert!(says(&frame, needle), "{needle}");
    }
    let available = rect(&frame, "marketplace_row_menu:source-1:git-review");
    let installed = rect(&frame, "marketplace_row_menu:source-1:installed");
    assert_eq!(available.x, installed.x, "the action column does not move");
    assert_eq!((available.width, available.height), (24.0, 24.0));
    let row = rect(&frame, "marketplace_details:source-1:git-review");
    assert_eq!(row.height, crate::ui_sizing::EXTENSION_ROW_HEIGHT);
    assert!(row.x + row.width <= available.x);

    // The row selects; its action is a target of its own.
    let point = Point::new(row.x + 40.0, row.y + 20.0);
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked {
            id: "marketplace_details:source-1:git-review".into()
        }]
    );
    let point = Point::new(available.x + 12.0, available.y + 12.0);
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked {
            id: "marketplace_row_menu:source-1:git-review".into()
        }]
    );
}

#[test]
fn installing_from_a_row_asks_for_the_scope_and_states_what_is_verified() {
    let mut state = starter_state();
    // The first row: its menu is anchored inside the viewport.
    let first = state.snapshot.items[0].id.clone();
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(!frame.regions.iter().any(|region| {
        region.id.starts_with("marketplace_install")
    }), "a row never installs with one click");

    state.menu_item = Some(format!("row:{first}"));
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(has(&frame, &format!("marketplace_install_user:{first}")));
    assert!(has(&frame, &format!("marketplace_install_workspace:{first}")));
    assert!(says(&frame, "Instalar para el usuario"));
    assert!(says(&frame, "Instalar en este proyecto"));
    assert!(says(&frame, "SHA-256"));
    assert!(says(&frame, "no se ejecuta código"));

    // While an operation runs no menu opens and the row shows its progress.
    state.busy = true;
    state.busy_item = Some(first.clone());
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(!has(&frame, &format!("marketplace_install_user:{first}")));
    assert!(!frame.regions.iter().any(|region| {
        region.id.starts_with("marketplace_row_menu:") && region.kind == HitKind::Button
    }));
}

#[test]
fn an_installed_row_is_managed_from_its_menu() {
    let mut state = sample_state();
    let id = "source-1:git-review";
    state.snapshot.items[0].installed_scope = Some(InstallScope::User);
    state.snapshot.items[0].enabled = true;
    state.menu_item = Some(format!("row:{id}"));
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(has(&frame, &format!("marketplace_toggle:{id}")));
    assert!(has(&frame, &format!("marketplace_uninstall:{id}")));
    assert!(!has(&frame, &format!("marketplace_update:{id}")));
    assert!(says(&frame, "Desactivar"));

    state.updates.insert(
        id.into(),
        SkillUpdate {
            item_id: id.into(),
            installed_revision: None,
            available_revision: "b".repeat(40),
        },
    );
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(has(&frame, &format!("marketplace_update:{id}")));
    assert!(says(&frame, "Actualización"));
}

#[test]
fn the_list_is_honest_while_loading_failing_and_finding_nothing() {
    let mut state = MarketplaceViewState {
        busy: true,
        ..Default::default()
    };
    let frame = layout(list(&state, 300.0), 300.0, 700.0);
    assert!(!says(&frame, "Sin skills"));
    assert!(!says(&frame, "Sin resultados"));

    state.busy = false;
    state.status = Some("Registro del marketplace inválido".into());
    state.status_error = true;
    let frame = layout(list(&state, 300.0), 300.0, 700.0);
    assert!(says(&frame, "No se pudo cargar el catálogo"));
    assert!(says(&frame, "Registro del marketplace inválido"));
    assert!(has(&frame, "marketplace_reload"));

    let mut state = sample_state();
    state.query = "nothing-matches".into();
    let frame = layout(list(&state, 300.0), 300.0, 700.0);
    assert!(says(&frame, "Sin resultados"));
    // The same text can still be searched online, on request.
    assert!(has(&frame, "marketplace_explore_search"));

    state.query.clear();
    state.filter = MarketplaceFilter::Installed;
    let frame = layout(list(&state, 300.0), 300.0, 700.0);
    assert!(says(&frame, "Todavía no instalaste skills"));
    assert!(has(&frame, "marketplace_filter:all"));
    assert!(has(&frame, "marketplace_filter_clear"));
    assert!(
        !says(&frame, "pulsa Enter para buscar"),
        "a filter of installed skills offers no online search"
    );

    state.snapshot.diagnostics.push("team-skills: carpeta no encontrada".into());
    state.filter = MarketplaceFilter::All;
    let frame = layout(list(&state, 300.0), 300.0, 700.0);
    assert!(says(&frame, "Un origen no se pudo leer"));
}

#[test]
fn a_skills_sh_search_shows_its_progress_failure_and_saved_copy() {
    let mut state = remote_state();
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(has(&frame, &format!("marketplace_details:{REMOTE}")));
    assert!(says(&frame, "example/skills · 23 instalaciones"));

    state.explore.page = None;
    state.explore.busy = true;
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(says(&frame, "Buscando «quality» en skills.sh…"));
    assert!(!has(&frame, "marketplace_explore_search"));

    state.explore.busy = false;
    state.explore.error = Some("No se pudo conectar con skills.sh.".into());
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(says(&frame, "No se pudo buscar en skills.sh"));
    assert!(says(&frame, "No se pudo conectar con skills.sh."));
    assert!(has(&frame, "marketplace_explore_retry"));

    state.explore.error = None;
    let mut page = remote_page();
    page.stale = true;
    page.warning = Some("No hay conexión. Estos resultados se guardaron anteriormente.".into());
    state.explore.page = Some(page);
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(says(&frame, "Copia guardada sin conexión"));
    assert!(has(&frame, &format!("marketplace_details:{REMOTE}")));

    // Nothing typed: the list says how a search is asked for.
    let frame = layout(list(&starter_state(), 320.0), 320.0, 2400.0);
    assert!(says(&frame, "pulsa Enter para buscar en skills.sh"));
}

fn detail(state: &MarketplaceViewState, id: &str, width: f32) -> Frame {
    detail_sized(state, id, width, 1400.0)
}

fn detail_sized(state: &MarketplaceViewState, id: &str, width: f32, height: f32) -> Frame {
    let item = state.find_item(id).unwrap();
    layout(item_detail(state, &item, width, None), width, height)
}

#[test]
fn the_detail_installs_to_a_named_scope_and_says_what_installing_verifies() {
    let mut state = starter_state();
    let install = format!("marketplace_install:{FRONTEND}");
    for width in [360.0, 900.0] {
        let frame = detail(&state, FRONTEND, width);
        assert!(has(&frame, &install), "{width}");
        assert!(has(&frame, &format!("marketplace_detail_menu:{FRONTEND}")));
        assert!(has(&frame, &format!("marketplace_open_source:{FRONTEND}")));
        for needle in [
            "frontend-design",
            "Instalar para el usuario",
            "Antes de instalar",
            "SHA-256",
            "Apache-2.0",
            "Anthropic · GitHub · rev. ",
            "Revisión fijada",
            "Qué instala",
            "SKILL.md",
            "Diseño y frontend",
        ] {
            assert!(says(&frame, needle), "{needle} at {width}");
        }
        for region in &frame.regions {
            assert!(
                region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.01,
                "{} at {width}",
                region.id
            );
        }
    }
    state.install_scope = InstallScope::Workspace;
    assert!(says(&detail(&state, FRONTEND, 900.0), "Instalar en este proyecto"));

    state.menu_item = Some(format!("detail:{FRONTEND}"));
    let frame = detail(&state, FRONTEND, 900.0);
    assert!(has(&frame, &format!("marketplace_install_user:{FRONTEND}")));
    assert!(has(&frame, &format!("marketplace_install_workspace:{FRONTEND}")));

    state.menu_item = None;
    state.busy = true;
    state.busy_item = Some(FRONTEND.into());
    let frame = detail(&state, FRONTEND, 900.0);
    assert!(says(&frame, "Instalando…"));
    assert!(!frame
        .regions
        .iter()
        .any(|region| region.id == install && region.kind == HitKind::Button));
}

#[test]
fn a_community_result_is_marked_unreviewed_with_its_real_install_count() {
    let state = remote_state();
    let frame = detail(&state, REMOTE, 900.0);
    for needle in [
        "sin revisar por openADE",
        "La licencia y las instrucciones se verifican al instalar",
        "Licencia no declarada",
        "23 registradas en skills.sh",
        "example/skills · skills.sh",
    ] {
        assert!(says(&frame, needle), "{needle}");
    }
    assert!(!says(&frame, "SHA-256"));
}

#[test]
fn the_detail_of_an_installed_item_toggles_updates_and_removes_through_the_same_routes() {
    let mut state = sample_state();
    let id = "source-1:git-review";
    state.snapshot.items[0].installed_scope = Some(InstallScope::Workspace);
    state.snapshot.items[0].enabled = true;
    state.provenance.insert(
        id.into(),
        SkillProvenance {
            repository: "owner/repository".into(),
            revision: "a".repeat(40),
            directory: "skills/git-review".into(),
            manifest_digest: None,
        },
    );
    let frame = detail(&state, id, 900.0);
    assert!(!has(&frame, &format!("marketplace_install:{id}")));
    assert!(!says(&frame, "Antes de instalar"));
    for needle in ["Instalada · Proyecto", "Activa", "Revisión instalada", "Desactivar"] {
        assert!(says(&frame, needle), "{needle}");
    }
    assert!(!has(&frame, &format!("marketplace_details_update:{id}")));

    state.updates.insert(
        id.into(),
        SkillUpdate {
            item_id: id.into(),
            installed_revision: Some("a".repeat(40)),
            available_revision: "b".repeat(40),
        },
    );
    let mut tree = UiTree::new(item_detail(&state, &state.find_item(id).unwrap(), 900.0, None));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(900.0, 1200.0));
    assert!(says(&frame, "Actualización disponible"));
    assert!(says(&frame, &"b".repeat(40)));
    assert_eq!(
        frame
            .text_layouts()
            .iter()
            .filter(|block| block.source == "Revisión instalada")
            .count(),
        1
    );
    for (control, action) in [
        ("marketplace_details_update", "marketplace_update"),
        ("marketplace_details_toggle", "marketplace_toggle"),
    ] {
        let rect = rect(&frame, &format!("{control}:{id}"));
        let point = Point::new(rect.x + 6.0, rect.y + 6.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: format!("{action}:{id}")
            }]
        );
    }
    state.menu_item = Some(format!("detail:{id}"));
    let frame = detail(&state, id, 900.0);
    assert!(has(&frame, &format!("marketplace_uninstall:{id}")));
}

#[test]
fn instructions_are_shown_when_on_disk_and_never_invented() {
    let mut state = starter_state();
    state.selected_item_id = Some(FRONTEND.into());
    let readme = |body| {
        Some(MarketplaceReadme {
            item_id: FRONTEND.into(),
            body,
        })
    };
    // Loading: neither text nor the "not downloaded" notice.
    state.readme = readme(None);
    let frame = detail(&state, FRONTEND, 900.0);
    assert!(!says(&frame, "se descargan al instalar"));

    state.readme = readme(Some(Ok(None)));
    let frame = detail(&state, FRONTEND, 900.0);
    assert!(says(&frame, "Las instrucciones se descargan al instalar"));
    assert!(has(&frame, &format!("marketplace_readme_source:{FRONTEND}")));

    state.readme = readme(Some(Ok(Some("## Steps\n\nRead the **diff** first.".into()))));
    let frame = detail(&state, FRONTEND, 900.0);
    assert!(says(&frame, "Steps"));
    assert!(says(&frame, "Read the"));
    assert!(!says(&frame, "se descargan al instalar"));

    state.readme = readme(Some(Err("permiso denegado".into())));
    let frame = detail(&state, FRONTEND, 900.0);
    assert!(says(&frame, "No se pudieron leer las instrucciones"));
    assert!(says(&frame, "permiso denegado"));

    // The instructions of another item are not shown for this one.
    state.readme = Some(MarketplaceReadme {
        item_id: "other".into(),
        body: Some(Ok(Some("## Wrong".into()))),
    });
    assert!(!says(&detail(&state, FRONTEND, 900.0), "Wrong"));
}

#[test]
fn a_long_readme_is_cut_at_the_renderer_limit_and_says_so() {
    let mut state = starter_state();
    let source: String = (0..600)
        .map(|index| format!("Paragraph number {index} of a very long skill.\n\n"))
        .collect();
    state.readme = Some(MarketplaceReadme {
        item_id: FRONTEND.into(),
        body: Some(Ok(Some(source))),
    });
    let frame = detail_sized(&state, FRONTEND, 900.0, 20_000.0);
    assert!(says(&frame, "Paragraph number 0 "));
    assert!(!says(&frame, "Paragraph number 599 "));
    assert!(says(&frame, "Documento extenso"));
}

#[test]
fn the_landing_page_starts_and_manages_sources_without_repeating_the_list() {
    let mut state = starter_state();
    state.snapshot.sources.push(MarketplaceSource {
        id: "source-1".into(),
        name: "team-skills".into(),
        location: "C:/team-skills".into(),
        kind: SourceKind::Local,
    });
    state
        .snapshot
        .diagnostics
        .push("team-skills: carpeta no encontrada".into());
    state.snapshot.items[0].installed_scope = Some(InstallScope::User);
    state.snapshot.items[0].enabled = true;
    for width in [360.0, 900.0] {
        let frame = layout(
            landing(&state, width, "marketplace_details:", Some(("Complementos", None))),
            width,
            1400.0,
        );
        let featured = frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("marketplace_featured:"))
            .count();
        assert_eq!(featured, 6, "one per publisher at {width}");
        assert!(!frame.regions.iter().any(|region| {
            region.id.starts_with("marketplace_row_menu:") || region.id == "marketplace_search"
        }));
        for id in [
            "marketplace_add_menu",
            "marketplace_refresh",
            "marketplace_check_updates",
            "marketplace_remove_source:source-1",
        ] {
            assert!(has(&frame, id), "{id} at {width}");
        }
        for needle in [
            "Catálogo recomendado",
            "skills.sh",
            "C:/team-skills",
            "carpeta no encontrada",
            "1 de 1 activas",
        ] {
            assert!(says(&frame, needle), "{needle} at {width}");
        }
    }
    // A featured card selects through the host's own route.
    let mut tree = UiTree::new(landing(&state, 900.0, "ide_extension:", None));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(900.0, 1400.0));
    let card = frame
        .regions
        .iter()
        .find(|region| region.id.starts_with("marketplace_featured:"))
        .unwrap();
    let item = card.id.trim_start_matches("marketplace_featured:").to_owned();
    let point = Point::new(card.rect.x + 20.0, card.rect.y + 20.0);
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked {
            id: format!("ide_extension:{item}")
        }]
    );
}

#[test]
fn the_standalone_page_is_the_same_list_beside_the_same_detail() {
    let mut state = sample_state();
    let id = "source-1:git-review";
    let row = format!("marketplace_details:{id}");
    let install = format!("marketplace_install:{id}");

    let frame = layout(marketplace_page(&state, 1000.0), 1000.0, 800.0);
    for control in ["marketplace_search", "marketplace_add_menu", row.as_str(), "marketplace_remove_source:source-1"] {
        assert!(has(&frame, control), "{control}");
    }
    assert!(!has(&frame, &install), "nothing selected: the landing page");

    state.selected_item_id = Some(id.into());
    let frame = layout(marketplace_page(&state, 1000.0), 1000.0, 800.0);
    assert!(has(&frame, &row) && has(&frame, &install));
    let list = rect(&frame, &row);
    let detail = rect(&frame, &install);
    assert!(list.x + list.width < detail.x, "list and detail side by side");
    assert!(!has(&frame, "marketplace_details_close"));

    // Too narrow for both: the detail with a way back, then the list.
    let frame = layout(marketplace_page(&state, 420.0), 420.0, 800.0);
    assert!(has(&frame, &install) && !has(&frame, &row));
    assert!(has(&frame, "marketplace_details_close"));
    state.selected_item_id = None;
    let frame = layout(marketplace_page(&state, 420.0), 420.0, 800.0);
    assert!(has(&frame, &row) && has(&frame, "marketplace_search"));
    assert!(has(&frame, "marketplace_remove_source:source-1"));
    for region in &frame.regions {
        assert!(region.rect.x + region.rect.width <= 420.01, "{}", region.id);
    }
}

#[test]
fn source_form_only_submits_a_selected_nonempty_location() {
    let mut state = MarketplaceViewState {
        add_menu_open: true,
        ..Default::default()
    };
    let frame = layout(marketplace_page(&state, 600.0), 600.0, 800.0);
    assert!(has(&frame, "marketplace_source_kind:local"));
    assert!(has(&frame, "marketplace_source_kind:github"));
    assert!(!has(&frame, "marketplace_source_submit"));
    // Selecting a menu option closes the menu and opens a centered form.
    state.add_menu_open = false;
    state.add_source_kind = Some(SourceKind::Github);
    state.source_draft = "https://github.com/owner/repo".into();
    let frame = layout(marketplace_page(&state, 600.0), 600.0, 800.0);
    for id in [
        "marketplace_source_draft",
        "marketplace_source_submit",
        "marketplace_source_cancel",
    ] {
        assert!(has(&frame, id), "{id}");
    }
    assert!(!has(&frame, "marketplace_source_kind:local"));
}

#[test]
fn source_notice_keeps_modal_submit_and_cancel_actions_reachable() {
    for width in [320.0, 920.0] {
        let mut state = sample_state();
        state.add_source_kind = Some(SourceKind::Local);
        state.source_draft = "C:/team-skills".into();
        state.status = Some("Comprueba la carpeta seleccionada antes de continuar.".into());
        let mut tree = UiTree::new(marketplace_page(&state, width));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(width, 360.0));
        for id in ["marketplace_source_cancel", "marketplace_source_submit"] {
            let control = frame
                .regions
                .iter()
                .find(|region| region.id == id && region.kind == HitKind::Button)
                .unwrap_or_else(|| panic!("missing {id} at {width}px"));
            assert!(control.rect.x >= 0.0 && control.rect.y >= 0.0);
            assert!(control.rect.x + control.rect.width <= width);
            assert!(control.rect.y + control.rect.height <= 360.0);
            let point = Point::new(control.rect.x + 4.0, control.rect.y + 4.0);
            assert_eq!(frame.hit_test(point), Some(id));
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked { id: id.into() }]
            );
        }
    }
}

#[test]
fn the_source_modal_takes_focus_closes_with_escape_and_returns_it() {
    let mut state = sample_state();
    state.add_menu_open = true;
    let mut tree = UiTree::new(marketplace_page(&state, 920.0));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(920.0, 800.0));
    assert!(tree.focus("marketplace_source_kind:local"));

    state.add_menu_open = false;
    state.add_source_kind = Some(SourceKind::Local);
    tree.set_root(marketplace_page(&state, 920.0));
    tree.layout(Size::new(920.0, 800.0));
    assert_eq!(tree.focused_id(), Some("marketplace_source_draft"));
    assert_eq!(
        tree.handle_event(UiEvent::Key {
            key: Key::Escape,
            shift: false,
            ctrl: false,
        }),
        vec![UiAction::Clicked {
            id: "marketplace_source_cancel".into(),
        }]
    );

    state.add_source_kind = None;
    tree.set_root(marketplace_page(&state, 920.0));
    tree.layout(Size::new(920.0, 800.0));
    assert_eq!(tree.focused_id(), Some("marketplace_add_menu"));
}

#[test]
fn offline_starter_details_reveal_provenance_without_installing_or_adding_sources() {
    let root = tempfile::tempdir().unwrap();
    let store = MarketplaceStore::new(root.path().join("data"), root.path().into());
    let state = MarketplaceViewState {
        snapshot: store.snapshot().unwrap(),
        selected_item_id: Some(FRONTEND.into()),
        ..Default::default()
    };
    let frame = layout(marketplace_page(&state, 1000.0), 1000.0, 1200.0);
    assert!(says(&frame, "https://github.com/anthropics/skills"));
    assert!(says(&frame, "skills/frontend-design"));
    assert!(says(&frame, "LICENSE.txt"));
    let after = store.snapshot().unwrap();
    assert!(after.sources.is_empty());
    assert!(after.items.iter().all(|item| item.installed_scope.is_none()));
    assert!(!root.path().join("data/marketplace/installed").exists());
}

#[test]
#[ignore = "export the extensions list, detail, landing and pages for manual visual review"]
fn export_marketplace_visual_review() {
    let output = std::path::PathBuf::from(format!(
        "{}/../../docs/ui-separation/shots/extensions-panel",
        env!("CARGO_MANIFEST_DIR")
    ));
    std::fs::create_dir_all(&output).unwrap();
    let save = |root: Element, width: f32, height: f32, name: &str| {
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
        let frame = tree.layout(Size::new(width, height));
        let path = output.join(format!("{name}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
            .unwrap();
        let parsed = resvg::usvg::Tree::from_str(
            &std::fs::read_to_string(&path).unwrap(),
            &crate::settings_ui::visual_qa::native_svg_options(),
        )
        .unwrap();
        let mut pixels = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
        resvg::render(
            &parsed,
            resvg::tiny_skia::Transform::identity(),
            &mut pixels.as_mut(),
        );
        pixels.save_png(path.with_extension("png")).unwrap();
        std::fs::remove_file(path).unwrap();
        context.end_pass().textures_delta.clear();
    };
    let update = |state: &mut MarketplaceViewState, id: &str| {
        state.updates.insert(
            id.into(),
            SkillUpdate {
                item_id: id.into(),
                installed_revision: Some("a".repeat(40)),
                available_revision: "b".repeat(40),
            },
        );
    };
    // A catalog with three installed skills, one outdated and one inactive.
    let populated = || {
        let mut state = starter_state();
        for (index, scope) in [(0, InstallScope::User), (5, InstallScope::Workspace), (9, InstallScope::User)] {
            state.snapshot.items[index].installed_scope = Some(scope);
            state.snapshot.items[index].enabled = index != 9;
        }
        let outdated = state.snapshot.items[5].id.clone();
        update(&mut state, &outdated);
        state.updates_checked = true;
        state
    };
    let readme = "# Frontend design\n\nUse this skill when the user asks for an interface with a deliberate visual identity.\n\n## When to use\n\n- A landing page, dashboard or component needs a point of view.\n- The request names a mood, a brand or a reference.\n\n## Steps\n\n1. Read the brief and name the **tone** in one sentence.\n2. Choose a type scale and a palette before any layout.\n3. Build the layout with real content.\n\n```css\n:root { --ink: #111; --accent: #c4513a; }\n```\n\n> Avoid defaults: system fonts, centered hero, purple gradients.\n\nSee [the guidelines](https://github.com/anthropics/skills) for examples.\n";
    let sidebar = |state: &MarketplaceViewState, width: f32| {
        let mut view = crate::view::ide_visual_tests::fixture();
        view.ide.extensions_open = true;
        view.marketplace = state.clone();
        Element::panel(crate::view::ide_activity_ui::extensions_sidebar(&view, width, 720.0))
            .background(theme::SIDEBAR)
    };
    for width in [280.0, 420.0] {
        let mut state = populated();
        state.selected_item_id = Some(FRONTEND.into());
        save(sidebar(&state, width), width, 720.0, &format!("list-{width:.0}-default"));
        state.query = "azure".into();
        save(sidebar(&state, width), width, 720.0, &format!("list-{width:.0}-searching"));
        state.explore = remote_state().explore;
        state.query = "quality".into();
        save(sidebar(&state, width), width, 720.0, &format!("list-{width:.0}-skills-sh"));
        let mut state = populated();
        state.filter = MarketplaceFilter::Installed;
        save(sidebar(&state, width), width, 720.0, &format!("list-{width:.0}-installed"));
        state.menu_item = Some(format!("row:{}", state.snapshot.items[5].id));
        save(sidebar(&state, width), width, 720.0, &format!("list-{width:.0}-manage-menu"));
        let mut state = populated();
        state.menu_item = Some(format!("row:{}", state.snapshot.items[1].id));
        save(sidebar(&state, width), width, 720.0, &format!("list-{width:.0}-install-menu"));
        let loading = MarketplaceViewState {
            busy: true,
            ..Default::default()
        };
        save(sidebar(&loading, width), width, 720.0, &format!("list-{width:.0}-loading"));
        let failed = MarketplaceViewState {
            status: Some("Registro del marketplace inválido: EOF while parsing".into()),
            status_error: true,
            ..Default::default()
        };
        save(sidebar(&failed, width), width, 720.0, &format!("list-{width:.0}-error"));
        let mut state = populated();
        state.query = "kubernetes".into();
        save(sidebar(&state, width), width, 720.0, &format!("list-{width:.0}-empty"));
        state.explore.query = "kubernetes".into();
        state.explore.error = Some("No se pudo conectar con skills.sh. Intenta de nuevo.".into());
        save(sidebar(&state, width), width, 720.0, &format!("list-{width:.0}-offline"));
    }
    let page = |state: &MarketplaceViewState, width: f32| {
        Element::panel(detail_or_landing(
            state,
            width,
            "marketplace_details:",
            Some(("Complementos", None)),
            None,
        ))
        .background(theme::BACKGROUND)
    };
    for width in [360.0, 900.0] {
        let mut state = populated();
        state.selected_item_id = Some(FRONTEND.into());
        state.snapshot.items.iter_mut().find(|item| item.id == FRONTEND).unwrap().installed_scope = None;
        state.readme = Some(MarketplaceReadme { item_id: FRONTEND.into(), body: Some(Ok(None)) });
        save(page(&state, width), width, 900.0, &format!("detail-{width:.0}-not-installed"));
        state.busy = true;
        state.busy_item = Some(FRONTEND.into());
        save(page(&state, width), width, 900.0, &format!("detail-{width:.0}-installing"));
        state.busy = false;
        state.busy_item = None;
        state.menu_item = Some(format!("detail:{FRONTEND}"));
        save(page(&state, width), width, 900.0, &format!("detail-{width:.0}-scope-menu"));
        state.menu_item = None;
        let item = state.snapshot.items.iter_mut().find(|item| item.id == FRONTEND).unwrap();
        item.installed_scope = Some(InstallScope::User);
        item.enabled = true;
        update(&mut state, FRONTEND);
        state.readme = Some(MarketplaceReadme { item_id: FRONTEND.into(), body: Some(Ok(Some(readme.into()))) });
        save(page(&state, width), width, 900.0, &format!("detail-{width:.0}-installed"));
        state.readme = Some(MarketplaceReadme {
            item_id: FRONTEND.into(),
            body: Some(Ok(Some(readme.repeat(40)))),
        });
        save(page(&state, width), width, 1600.0, &format!("detail-{width:.0}-long-readme"));
        let mut remote = remote_state();
        remote.selected_item_id = Some(REMOTE.into());
        remote.readme = Some(MarketplaceReadme { item_id: REMOTE.into(), body: Some(Ok(None)) });
        save(page(&remote, width), width, 900.0, &format!("detail-{width:.0}-skills-sh"));
        let mut state = populated();
        state.snapshot.sources.push(MarketplaceSource {
            id: "source-1".into(),
            name: "vercel-labs/skills".into(),
            location: "https://github.com/vercel-labs/skills.git".into(),
            kind: SourceKind::Github,
        });
        state.snapshot.sources.push(MarketplaceSource {
            id: "source-2".into(),
            name: "team-skills".into(),
            location: "/home/dev/team-skills".into(),
            kind: SourceKind::Local,
        });
        state.snapshot.diagnostics.push("team-skills: No such file or directory (os error 2)".into());
        save(page(&state, width), width, 900.0, &format!("landing-{width:.0}"));
    }
    for width in [420.0, 1000.0, 1440.0] {
        let mut state = populated();
        save(
            Element::panel(marketplace_page(&state, width)).background(theme::BACKGROUND),
            width,
            860.0,
            &format!("page-{width:.0}-landing"),
        );
        state.selected_item_id = Some(FRONTEND.into());
        state.readme = Some(MarketplaceReadme { item_id: FRONTEND.into(), body: Some(Ok(Some(readme.into()))) });
        save(
            Element::panel(marketplace_page(&state, width)).background(theme::BACKGROUND),
            width,
            860.0,
            &format!("page-{width:.0}-detail"),
        );
        state.selected_item_id = None;
        state.add_source_kind = Some(SourceKind::Github);
        save(
            Element::panel(marketplace_page(&state, width)).background(theme::BACKGROUND),
            width,
            860.0,
            &format!("page-{width:.0}-add-source"),
        );
    }
    for (width, sidebar_open, phase) in [
        (1440.0, true, "detail"),
        (1440.0, true, "landing"),
        (1440.0, true, "installed"),
        (1000.0, true, "detail"),
        (1440.0, false, "detail"),
    ] {
        let mut state = crate::view::ide_visual_tests::fixture();
        state.ide.activity = crate::ide_workspace::IdeActivity::Extensions;
        state.ide.explorer_open = sidebar_open;
        state.ide.agent_open = false;
        state.ide.extensions_open = true;
        state.marketplace = populated();
        if phase != "landing" {
            let id = if phase == "installed" {
                state.marketplace.snapshot.items[5].id.clone()
            } else {
                FRONTEND.to_owned()
            };
            state.marketplace.readme = Some(MarketplaceReadme {
                item_id: id.clone(),
                body: Some(if phase == "installed" { Ok(Some(readme.into())) } else { Ok(None) }),
            });
            if phase == "detail" {
                state.marketplace.snapshot.items.iter_mut().find(|item| item.id == id).unwrap().installed_scope = None;
            }
            state.marketplace.selected_item_id = Some(id);
        }
        save(
            crate::view::build_window_view(&state, width, 900.0),
            width,
            900.0,
            &format!("ide-{width:.0}-{phase}{}", if sidebar_open { "" } else { "-no-sidebar" }),
        );
    }
}
