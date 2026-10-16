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
    assert!(
        groups.iter().flat_map(|group| &group.items).all(|item| {
            crate::marketplace::starter_category(&item.id) == Some("Documentación")
        })
    );

    state.category = None;
    state.explore.page = None;
    state.query = "  FRONTEND-design ".into();
    let groups = catalog_groups(&state);
    assert_eq!(
        groups.iter().map(|group| group.items.len()).sum::<usize>(),
        1
    );
    state.query = "anthropic".into();
    assert!(
        catalog_groups(&state)
            .iter()
            .flat_map(|group| &group.items)
            .all(|item| item.source_name.contains("Anthropic"))
    );
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
    let tall =
        |state: &MarketplaceViewState| layout(list_sized(state, 320.0, 3000.0), 320.0, 3000.0);
    let frame = tall(&state);
    assert_eq!(rows(&frame), GROUP_STEP);
    assert!(says(
        &frame,
        &format!("Mostrar más ({} restantes)", starters - GROUP_STEP)
    ));
    // The header counts every match, not only the rows built.
    assert!(says(&frame, &starters.to_string()));
    assert!(
        !frame
            .regions
            .iter()
            .any(|region| region.id.contains("page"))
    );

    state.visible += GROUP_STEP;
    let frame = tall(&state);
    assert_eq!(rows(&frame), starters);
    assert!(!says(&frame, "Mostrar más"));
}

#[test]
fn a_row_says_what_the_item_is_and_keeps_its_action_in_place() {
    let mut state = sample_state();
    state.snapshot.items.push(skill(
        "source-1:installed",
        "installed-skill",
        Some(InstallScope::Workspace),
    ));
    let mut tree = UiTree::new(list(&state, 300.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(300.0, 700.0));
    for needle in [
        "git-review",
        "Review code changes",
        "team-skills · Carpeta local",
        "Proyecto",
    ] {
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
    assert!(
        !frame
            .regions
            .iter()
            .any(|region| { region.id.starts_with("marketplace_install") }),
        "a row never installs with one click"
    );

    state.menu_item = Some(format!("row:{first}"));
    let frame = layout(list(&state, 320.0), 320.0, 700.0);
    assert!(has(&frame, &format!("marketplace_install_user:{first}")));
    assert!(has(
        &frame,
        &format!("marketplace_install_workspace:{first}")
    ));
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

    state
        .snapshot
        .diagnostics
        .push("team-skills: carpeta no encontrada".into());
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
    layout(item_detail(state, &item, width, None, None), width, height)
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
    assert!(says(
        &detail(&state, FRONTEND, 900.0),
        "Instalar en este proyecto"
    ));

    state.menu_item = Some(format!("detail:{FRONTEND}"));
    let frame = detail(&state, FRONTEND, 900.0);
    assert!(has(&frame, &format!("marketplace_install_user:{FRONTEND}")));
    assert!(has(
        &frame,
        &format!("marketplace_install_workspace:{FRONTEND}")
    ));

    state.menu_item = None;
    state.busy = true;
    state.busy_item = Some(FRONTEND.into());
    let frame = detail(&state, FRONTEND, 900.0);
    assert!(says(&frame, "Instalando…"));
    assert!(
        !frame
            .regions
            .iter()
            .any(|region| region.id == install && region.kind == HitKind::Button)
    );
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
    for needle in [
        "Instalada · Proyecto",
        "Activa",
        "Revisión instalada",
        "Desactivar",
    ] {
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
    let mut tree = UiTree::new(item_detail(
        &state,
        &state.find_item(id).unwrap(),
        900.0,
        None,
        None,
    ));
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
    assert!(has(
        &frame,
        &format!("marketplace_readme_source:{FRONTEND}")
    ));

    state.readme = readme(Some(Ok(Some(
        "## Steps\n\nRead the **diff** first.".into(),
    ))));
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
fn the_sources_page_manages_provenance_without_repeating_the_skills_catalog() {
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
    for width in [280.0, 360.0, 900.0] {
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(source_manager_body(&state, width));
        let frame = tree.layout(Size::new(width, 1400.0));
        assert!(!frame.regions.iter().any(|region| {
            region.id.starts_with("marketplace_featured:")
                || region.id.starts_with("marketplace_details:")
                || region.id.starts_with("marketplace_row_menu:")
                || region.id == "marketplace_search"
        }));
        assert!(!says(&frame, "Para empezar") && !says(&frame, "Instaladas"));
        assert!(!has(&frame, "marketplace_check_updates"));
        assert!(has(&frame, "marketplace_remove_source:source-1"));
        assert!(!has(&frame, "marketplace_add_menu") && !has(&frame, "marketplace_refresh"));
        for needle in [
            "Catálogo recomendado",
            "skills.sh",
            "C:/team-skills",
            "carpeta no encontrada",
        ] {
            assert!(says(&frame, needle), "{needle} at {width}");
        }
        for region in &frame.regions {
            assert!(
                region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1,
                "{} at {width}",
                region.id
            );
        }
        let labels = frame.text_layouts();
        let title = labels
            .iter()
            .find(|block| block.source == "Catálogo recomendado")
            .expect("source name wraps rather than losing its identity");
        let visible = title
            .runs
            .iter()
            .filter(|run| run.visible.is_some())
            .map(|run| run.text.as_str())
            .collect::<String>();
        assert_eq!(
            visible
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>(),
            "Catálogorecomendado",
            "full source name must remain readable at {width}"
        );
        for run in &title.runs {
            assert!(run.rect.x >= 0.0 && run.rect.x + run.rect.width <= width + 0.1);
            assert!(
                run.visible
                    .is_some_and(|visible| visible.height >= run.rect.height - 0.1)
            );
        }
        context.end_pass().textures_delta.clear();
    }
    // Source management retains the established refresh action.
    state.sources_open = true;
    let mut tree = UiTree::new(marketplace_page(&state, 900.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(900.0, 1400.0));
    let refresh = rect(&frame, "marketplace_refresh");
    let point = Point::new(
        refresh.x + refresh.width / 2.0,
        refresh.y + refresh.height / 2.0,
    );
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked {
            id: "marketplace_refresh".into()
        }]
    );
}

#[test]
fn installed_skills_keep_their_update_check_in_the_catalog_toolbar() {
    let mut state = sample_state();
    assert!(!has(
        &layout(list(&state, 320.0), 320.0, 700.0),
        "marketplace_updates_check"
    ));
    state.snapshot.items[0].installed_scope = Some(InstallScope::User);
    let mut tree = UiTree::new(list(&state, 320.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(320.0, 700.0));
    let check = rect(&frame, "marketplace_updates_check");
    let point = Point::new(check.x + check.width / 2.0, check.y + check.height / 2.0);
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked {
            id: "marketplace_check_updates".into()
        }]
    );
    assert!(tree.focus("marketplace_updates_check"));
    assert_eq!(
        tree.handle_event(UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false
        }),
        vec![UiAction::Clicked {
            id: "marketplace_check_updates".into()
        }]
    );
    state.busy = true;
    let mut disabled = UiTree::new(list(&state, 320.0));
    disabled.set_reduced_motion(true);
    let frame = disabled.layout(Size::new(320.0, 700.0));
    assert!(
        !frame.regions.iter().any(
            |region| region.id == "marketplace_updates_check" && region.kind == HitKind::Button
        )
    );
    assert!(
        frame
            .access
            .iter()
            .any(|node| node.id == "marketplace_updates_check"
                && node.disabled
                && !node.actionable)
    );
    disabled.handle_event(UiEvent::PointerDown(point));
    assert!(disabled.handle_event(UiEvent::PointerUp(point)).is_empty());
    assert!(!disabled.focus("marketplace_updates_check"));
    assert!(
        disabled
            .handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            })
            .is_empty()
    );
}

#[test]
fn the_catalog_fills_the_page_with_responsive_cards_and_opens_detail_in_place() {
    let mut state = sample_state();
    state.snapshot.items = (0..7)
        .map(|index| skill(&format!("source-1:skill-{index}"), &format!("Skill {index}"), None))
        .collect();
    for (width, columns) in [(320.0, 1), (900.0, 2), (1400.0, 3)] {
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
            marketplace_page(&state, width),
        );
        let frame = tree.layout(Size::new(width, 1200.0));
        assert!(has(&frame, "marketplace_search"));
        assert!(has(&frame, "marketplace_sources_open"));
        assert!(!has(&frame, "marketplace_add_menu"));
        assert!(!has(&frame, "marketplace_remove_source:source-1"));
        assert!(!frame.access.iter().any(|node| node.role == openide_ui::Role::Tab));
        let cards = (0..7)
            .map(|index| rect(&frame, &format!("marketplace_details:source-1:skill-{index}")))
            .collect::<Vec<_>>();
        assert_eq!(cards.iter().filter(|card| (card.y - cards[0].y).abs() < 0.1).count(), columns);
        assert!(cards[columns].y > cards[0].y + cards[0].height);
        for card in &cards {
            assert!(card.x >= 0.0 && card.x + card.width <= width + 0.1);
        }
        let first = cards[0];
        let point = Point::new(first.x + first.width / 2.0, first.y + first.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(tree.handle_event(UiEvent::PointerUp(point)), vec![UiAction::Clicked {
            id: "marketplace_details:source-1:skill-0".into(),
        }]);
        context.end_pass().textures_delta.clear();

        let mut selected = state.clone();
        selected.selected_item_id = Some("source-1:skill-0".into());
        let frame = layout(marketplace_page(&selected, width), width, 1200.0);
        assert!(has(&frame, "marketplace_install:source-1:skill-0"));
        assert!(has(&frame, "marketplace_details_close"));
        assert!(says(&frame, "Volver al catálogo"));
        assert!(!has(&frame, "marketplace_search"));
        assert!(!has(&frame, "marketplace_details:source-1:skill-0"));
    }
}

#[test]
fn the_sources_modal_traps_focus_and_returns_it_to_catalog_or_detail() {
    for width in [320.0, 900.0] {
        for selected in [false, true] {
            let mut state = sample_state();
            state.selected_item_id = selected.then(|| "source-1:git-review".into());
            let size = Size::new(width, 800.0);
            let root = marketplace_page_with_navigation(&state, width,
                Some(crate::view::sidebar_reopen_control(theme::BACKGROUND)));
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
            let frame = tree.layout(size);
            assert!(has(&frame, "toggle_sidebar_reopen"));
            for id in ["toggle_sidebar_reopen", "marketplace_sources_open"] {
                let target = rect(&frame, id);
                assert!(target.x >= 0.0 && target.x + target.width <= width + 0.1);
            }
            assert!(tree.focus("marketplace_sources_open"));
            assert_eq!(tree.handle_event(UiEvent::Key {
                key: Key::Enter, shift: false, ctrl: false,
            }), vec![UiAction::Clicked { id: "marketplace_sources_open".into() }]);
            state.sources_open = true;
            tree.set_root(marketplace_page(&state, width));
            let frame = tree.layout(size);
            assert!(frame.access.iter().any(|node| node.id == "marketplace_sources_close:dialog"
                && node.role == openide_ui::Role::Dialog));
            assert_eq!(tree.focused_id(), Some("marketplace_add_menu"));
            // focus() is an explicit host override. Real keyboard navigation
            // must cycle within the dialog, and Enter may activate only its controls.
            let allowed = [
                ("marketplace_sources_close:close", "marketplace_sources_close"),
                ("marketplace_remove_source:source-1", "marketplace_remove_source:source-1"),
                ("marketplace_refresh", "marketplace_refresh"),
                ("marketplace_add_menu", "marketplace_add_menu"),
            ];
            let mut visited = std::collections::BTreeSet::new();
            for reverse in [false, true] {
                for _ in 0..allowed.len() * 2 {
                    assert!(tree.handle_event(UiEvent::Key {
                        key: Key::Tab, shift: reverse, ctrl: false,
                    }).is_empty());
                    let focused = tree.focused_id().expect("modal keeps keyboard focus");
                    let (id, action) = allowed.iter().find(|(id, _)| *id == focused)
                        .unwrap_or_else(|| panic!("focus escaped the modal to {focused}"));
                    visited.insert(*id);
                    assert_eq!(tree.handle_event(UiEvent::Key {
                        key: Key::Enter, shift: false, ctrl: false,
                    }), vec![UiAction::Clicked { id: (*action).into() }]);
                }
            }
            assert_eq!(visited.len(), allowed.len());
            let opener = rect(&frame, "marketplace_sources_open");
            let point = Point::new(opener.x + opener.width / 2.0, opener.y + opener.height / 2.0);
            let mut actions = tree.handle_event(UiEvent::PointerDown(point));
            actions.extend(tree.handle_event(UiEvent::PointerUp(point)));
            assert_eq!(actions, vec![UiAction::Clicked { id: "marketplace_sources_close".into() }],
                "the backdrop consumes the click over the covered opener");
            assert_eq!(tree.handle_event(UiEvent::Key {
                key: Key::Escape, shift: false, ctrl: false,
            }), vec![UiAction::Clicked { id: "marketplace_sources_close".into() }]);
            state.sources_open = false;
            tree.set_root(marketplace_page(&state, width));
            tree.layout(size);
            assert_eq!(tree.focused_id(), Some("marketplace_sources_open"));
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn source_management_remains_dismissible_while_a_refresh_is_running() {
    let state = MarketplaceViewState { sources_open: true, busy: true, ..sample_state() };
    let mut tree = UiTree::new(marketplace_page(&state, 900.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(900.0, 700.0));
    assert_eq!(tree.focused_id(), Some("marketplace_sources_close:close"));
    assert!(frame.access.iter().any(|node| node.id == "marketplace_add_menu" && node.disabled));
    assert_eq!(tree.handle_event(UiEvent::Key {
        key: Key::Escape, shift: false, ctrl: false,
    }), vec![UiAction::Clicked { id: "marketplace_sources_close".into() }]);
}

#[test]
fn the_full_catalog_keeps_each_group_bounded_and_its_more_action() {
    let mut state = sample_state();
    state.snapshot.items = (0..100)
        .map(|index| skill(&format!("source-1:skill-{index}"), &format!("Skill {index}"), None))
        .collect();
    state.visible_by.insert("sources".into(), 3);
    let frame = layout(marketplace_page(&state, 1400.0), 1400.0, 900.0);
    for index in 0..3 {
        assert!(has(&frame, &format!("marketplace_details:source-1:skill-{index}")));
    }
    assert!(!has(&frame, "marketplace_details:source-1:skill-3"));
    assert!(has(&frame, "marketplace_more:sources"));
    assert!(says(&frame, "Mostrar más (97 restantes)"));
}

#[test]
fn catalog_navigation_and_source_controls_fit_narrow_and_wide_panes() {
    let mut state = sample_state();
    for width in [320.0, 900.0, 1400.0] {
        let root = marketplace_page_with_navigation(&state, width,
            Some(crate::view::sidebar_reopen_control(theme::BACKGROUND)));
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
        let frame = tree.layout(Size::new(width, 800.0));
        let opener = rect(&frame, "marketplace_sources_open");
        assert!(opener.x >= 0.0 && opener.x + opener.width <= width + 0.1);
        assert!(tree.focus("marketplace_sources_open"));
        assert_eq!(tree.handle_event(UiEvent::Key {
            key: Key::Enter, shift: false, ctrl: false,
        }), vec![UiAction::Clicked { id: "marketplace_sources_open".into() }]);
        state.sources_open = true;
        tree.set_root(marketplace_page(&state, width));
        let frame = tree.layout(Size::new(width, 800.0));
        for id in ["marketplace_sources_close:close", "marketplace_refresh", "marketplace_add_menu"] {
            let target = rect(&frame, id);
            assert!(target.x >= 0.0 && target.x + target.width <= width + 0.1, "{id} at {width}");
        }
        state.sources_open = false;
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn the_ide_empty_detail_does_not_repeat_the_catalog_or_source_manager() {
    let state = sample_state();
    for width in [320.0, 900.0] {
        let root = detail_or_landing(&state, width, "marketplace_details:", None, None);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
        let frame = tree.layout(Size::new(width, 600.0));
        assert!(says(&frame, "Elegí una skill de la lista"));
        assert!(says(&frame, "Consulta sus instrucciones, procedencia y opciones de instalación."));
        assert!(!has(&frame, "marketplace_search"));
        assert!(!has(&frame, "marketplace_sources_open"));
        assert!(!frame.regions.iter().any(|region| region.id.starts_with("marketplace_details:")));
        for block in frame.text_layouts() {
            for run in &block.runs {
                assert!(run.rect.x >= 0.0 && run.rect.x + run.rect.width <= width + 0.1);
            }
        }
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn source_form_only_submits_a_selected_nonempty_location() {
    let mut state = MarketplaceViewState {
        sources_open: true,
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
    state.sources_open = true;
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
    assert!(
        after
            .items
            .iter()
            .all(|item| item.installed_scope.is_none())
    );
    assert!(!root.path().join("data/marketplace/installed").exists());
}

#[test]
#[ignore = "export six native Marketplace states for manual visual review"]
fn export_marketplace_visual_review() {
    let output = std::env::var_os("OPENIDE_MARKETPLACE_VISUAL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(format!(
            "{}/../../docs/ui-separation/shots/extensions-panel", env!("CARGO_MANIFEST_DIR")
        )));
    std::fs::create_dir_all(&output).unwrap();
    for (width, phase) in [(320.0, "catalog"), (900.0, "catalog"), (1400.0, "catalog"),
        (320.0, "sources"), (900.0, "detail"), (1400.0, "installed")] {
        let mut state = starter_state();
        for index in [0, 5, 9] {
            state.snapshot.items[index].installed_scope = Some(InstallScope::User);
            state.snapshot.items[index].enabled = index != 9;
        }
        if phase == "sources" {
            state.sources_open = true;
            state.snapshot.sources.push(MarketplaceSource {
                id: "source-1".into(), name: "team-skills".into(),
                location: "/workspace/team-skills".into(), kind: SourceKind::Local,
            });
        } else if phase == "detail" {
            state.selected_item_id = Some(FRONTEND.into());
            state.readme = Some(MarketplaceReadme { item_id: FRONTEND.into(),
                body: Some(Ok(Some("# Frontend design\n\nCreate an interface with a deliberate visual identity.\n\n## Workflow\n\n1. Read the brief.\n2. Build the interface.\n3. Check it at narrow and wide widths.".into()))),
            });
        } else if phase == "installed" {
            state.filter = MarketplaceFilter::Installed;
        }
        let height = 900.0;
        let root = Element::panel(marketplace_page(&state, width)).background(theme::BACKGROUND);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
        let frame = tree.layout(Size::new(width, height));
        let path = output.join(format!("marketplace-{width:.0}-{phase}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height).unwrap();
        let parsed = resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(),
            &crate::settings_ui::visual_qa::native_svg_options()).unwrap();
        let mut pixels = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
        resvg::render(&parsed, resvg::tiny_skia::Transform::identity(), &mut pixels.as_mut());
        pixels.save_png(path.with_extension("png")).unwrap();
        std::fs::remove_file(path).unwrap();
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn marketplace_is_a_catalog_and_management_remains_in_settings() {
    use crate::settings_ui::SettingsPage;
    let mut state = crate::view::tests::empty_state();
    state.main_page = crate::view::MainPage::Marketplace;
    state.marketplace = sample_state();
    let size = Size::new(1200.0, 800.0);
    let frame = layout(crate::view::build_view_sized(&state, size.width, size.height), size.width, size.height);
    assert!(has(&frame, "marketplace_search"));
    assert!(has(&frame, "marketplace_sources_open"));
    assert!(!frame.regions.iter().any(|region| region.id.starts_with("marketplace_tab:")));
    assert!(!has(&frame, "mcp_new") && !has(&frame, "plugins_refresh"));
    assert!(!state.shows(SettingsPage::Mcp) && !state.shows(SettingsPage::Plugins));
    state.show_settings = true;
    state.settings_page = SettingsPage::Mcp;
    assert!(state.shows(SettingsPage::Mcp) && !state.shows(SettingsPage::Plugins));
    state.settings_page = SettingsPage::Plugins;
    assert!(state.shows(SettingsPage::Plugins) && !state.shows(SettingsPage::Mcp));
}

#[test]
fn appearance_offers_built_in_and_local_themes_in_one_native_selector() {
    use crate::settings_ui::SettingsPage;
    let mut state = crate::view::tests::empty_state();
    state.show_settings = true;
    state.settings_page = SettingsPage::Appearance;
    let local = crate::custom_theme::CustomTheme::new(
        "Tema local",
        crate::custom_theme::ThemeAppearance::Dark,
    );
    state.appearance.active_custom_theme = Some(local.id.clone());
    state.appearance.custom_themes.push(local.clone());
    let size = Size::new(1200.0, 800.0);
    let trigger = "settings_choice_toggle:appearance_theme_selection";
    let mut tree = UiTree::new(crate::view::build_view_sized(
        &state,
        size.width,
        size.height,
    ));
    tree.set_reduced_motion(true);
    let frame = tree.layout(size);
    assert!(has(&frame, trigger));
    assert!(
        !has(&frame, "settings_theme:dark"),
        "closed selectors hide their options"
    );
    let said = frame.access.iter().find(|node| node.id == trigger).unwrap();
    assert_eq!(said.role, openide_ui::Role::ComboBox);
    assert_eq!(said.name, "Tema");
    assert_eq!(said.expanded, Some(false));
    assert!(
        said.value_text
            .as_deref()
            .is_some_and(|label| !label.is_empty())
    );
    assert!(tree.focus(trigger));
    assert_eq!(
        tree.handle_event(UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false
        }),
        vec![UiAction::Clicked { id: trigger.into() }],
    );

    state.settings_choice_menu_open = Some("appearance_theme_selection".into());
    tree.set_root(crate::view::build_view_sized(
        &state,
        size.width,
        size.height,
    ));
    let frame = tree.layout(size);
    let local_action = format!("appearance_theme_apply:{}", local.id);
    for action in [
        "settings_theme:system",
        "settings_theme:light",
        "settings_theme:dark",
        "settings_theme:high_contrast_light",
        "settings_theme:high_contrast_dark",
        local_action.as_str(),
    ] {
        assert!(has(&frame, action), "missing option {action}");
        assert!(
            tree.focus(action),
            "option {action} must accept keyboard focus"
        );
    }
    for name in [
        "Sistema",
        "Claro",
        "Oscuro",
        "Claro con contraste",
        "Oscuro con contraste",
        "Tema local · Personalizado",
    ] {
        assert!(says(&frame, name), "missing label {name}");
    }
    let selected = frame
        .access
        .iter()
        .filter(|node| {
            node.selected == Some(true)
                && (node.id.starts_with("settings_theme:") || node.id == local_action)
        })
        .count();
    assert_eq!(selected, 1, "the menu must mark exactly the active theme");
    assert_eq!(
        tree.handle_event(UiEvent::Key {
            key: Key::Escape,
            shift: false,
            ctrl: false
        }),
        vec![UiAction::Clicked {
            id: "settings_choice_dismiss".into()
        }],
    );
}

#[test]
fn settings_are_found_by_what_a_row_does_and_the_editor_has_its_page() {
    use crate::settings_ui::SettingsPage;
    let mut state = crate::view::tests::empty_state();
    state.show_settings = true;
    let size = Size::new(1200.0, 800.0);
    let shown = |state: &crate::view::ViewState, id: &str| {
        has(
            &UiTree::new(crate::view::build_view_sized(
                state,
                size.width,
                size.height,
            ))
            .layout(size),
            id,
        )
    };
    // A word of a row, in either language, finds the page that holds it.
    for (query, page) in [
        ("idioma", "appearance"),
        ("language", "appearance"),
        ("minimapa", "editor"),
        ("sandbox", "agent"),
    ] {
        state.settings_nav_query = query.into();
        assert!(shown(&state, &format!("settings_page:{page}")), "{query}");
        assert!(!shown(&state, "settings_page:terminal"), "{query}");
    }
    state.settings_nav_query.clear();
    state.settings_page = SettingsPage::Editor;
    // The page is taller than the window: its controls are on its first or last screen.
    let mut editor = UiTree::new(crate::view::build_view_sized(&state, size.width, size.height));
    editor.set_reduced_motion(true);
    let top = editor.layout(size);
    editor.scroll_to_end("settings_page:Editor");
    let bottom = editor.layout(size);
    for control in [
        "editor_wrap",
        "editor_minimap",
        "editor_view:line_numbers",
        "editor_view:ruler",
        "editor_view:caret",
        "editor_view:trim_on_save",
        "editor_pairs",
        "editor_indent_kind",
        "editor_indent_width",
    ] {
        assert!(has(&top, control) || has(&bottom, control), "{control}");
    }
    state.settings_page = SettingsPage::Appearance;
    let mut appearance = UiTree::new(crate::view::build_view_sized(
        &state,
        size.width,
        size.height,
    ));
    appearance.set_reduced_motion(true);
    appearance.layout(size);
    appearance.scroll_to_end("settings_page:Appearance");
    let frame = appearance.layout(size);
    assert!(has(&frame, "settings_language:es") && has(&frame, "settings_language:en"));
    assert!(appearance.focus("settings_language:es") && appearance.focus("settings_language:en"));
    state.settings_page = SettingsPage::Data;
    assert!(shown(&state, "settings_open_data_folder"));
}
