use super::*;
use crate::ide_workspace::WindowMode;
use crate::tour::TourId;
use openide_ui::{Key, UiEvent};

const WIDE: Size = Size::new(1440.0, 900.0);
const NARROW: Size = Size::new(520.0, 680.0);

fn app_at(root: &std::path::Path, size: Size) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.to_owned());
    app.viewport_width = size.width;
    app.viewport_height = size.height;
    app.reduced_motion = true;
    app.tree.set_reduced_motion(true);
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app
}

fn click(app: &mut AgentsWindow, id: &str) {
    app.apply_action(UiAction::Clicked { id: id.into() }, &egui::Context::default());
}

/// One frame as the window paints it: laid out, then settled.
fn frame(app: &mut AgentsWindow) -> UiFrame {
    let size = Size::new(app.viewport_width, app.viewport_height);
    let frame = app.layout_workspace_frame(size);
    app.settle_tour(frame, size, &egui::Context::default())
}

fn has(frame: &UiFrame, id: &str) -> bool {
    frame.regions.iter().any(|region| region.id == id)
}

fn step(app: &AgentsWindow) -> Option<&'static str> {
    let active = app.view.tour.active?;
    Some(active.tour.steps()[active.step].id)
}

fn record(app: &AgentsWindow, tour: TourId) -> Option<(u32, bool)> {
    SettingsPreferences::load(&app.data_root)
        .tours
        .get(tour.key())
        .map(|record| (record.version, record.completed))
}

#[test]
fn each_window_mode_offers_its_tour_once_and_a_newer_version_again() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), WIDE);
    assert!(app.view.tour.invites.is_empty(), "nothing is offered before a frame");
    assert!(app.sync_tour());
    assert_eq!(app.view.tour.invites, vec![TourId::Agents]);
    assert!(app.view.tour.active.is_none(), "an offer never starts the tour");
    assert_eq!(record(&app, TourId::Agents), Some((1, false)));
    let shown = frame(&mut app);
    assert!(has(&shown, "tour_invite_decline") && has(&shown, "tour_start:agents"));
    assert!(!has(&shown, "popover_surface:tour"));
    assert!(!app.sync_tour(), "the same frame asks nothing new");

    click(&mut app, "tour_invite_decline");
    assert!(!app.sync_tour());
    assert!(!has(&frame(&mut app), "tour_invite_decline"));

    // The other mode has its own tour, offered the first time it is shown.
    click(&mut app, "window_mode_toggle");
    assert!(app.sync_tour());
    assert_eq!(app.view.tour.invites, vec![TourId::Ide]);
    assert!(has(&frame(&mut app), "tour_start:ide"));

    // A later launch offers neither again.
    let mut later = app_at(root.path(), WIDE);
    assert!(!later.sync_tour());
    later.view.ide.mode = WindowMode::Ide;
    assert!(!later.sync_tour());
    assert!(later.view.tour.invites.is_empty());

    // A record of an older version is offered again; Settings resets both.
    let mut older = later.settings_preferences.clone();
    older.tours.get_mut("ide").unwrap().version = 0;
    older.save(root.path()).unwrap();
    let mut bumped = app_at(root.path(), WIDE);
    bumped.view.ide.mode = WindowMode::Ide;
    assert!(bumped.sync_tour());
    assert_eq!(bumped.view.tour.invites, vec![TourId::Ide]);
    click(&mut bumped, "tour_reset");
    assert_eq!(record(&bumped, TourId::Ide), None);
    assert!(bumped.sync_tour());
}

#[test]
fn settings_and_other_pages_wait_to_offer_until_the_workspace_is_shown() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), WIDE);
    app.view.show_settings = true;
    assert!(!app.sync_tour());
    assert_eq!(record(&app, TourId::Agents), None);
    app.view.show_settings = false;
    assert!(app.sync_tour());
}

#[test]
fn next_back_close_and_skip_walk_the_tour_and_record_how_it_ended() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), WIDE);
    click(&mut app, "tour_start:agents");
    assert_eq!(step(&app), Some("new-task"));
    let shown = frame(&mut app);
    assert!(has(&shown, "popover_surface:tour") && !has(&shown, "tour:back"));
    assert!(app.tour_showing());
    assert!(app.tree.focused_id() == Some("tour:next"));

    // The keyboard reaches the same actions through the bubble.
    let key = |key| UiEvent::Key { key, shift: false, ctrl: false };
    for (pressed, action) in [
        (Key::ArrowRight, "tour:next"),
        (Key::Enter, "tour:next"),
        (Key::Escape, "tour"),
    ] {
        assert_eq!(
            app.tree.handle_event(key(pressed)),
            vec![UiAction::Clicked { id: action.into() }]
        );
    }

    click(&mut app, "tour:next");
    assert_eq!(step(&app), Some("tasks"));
    assert!(has(&frame(&mut app), "tour:back"));
    click(&mut app, "tour:back");
    click(&mut app, "tour:back");
    assert_eq!(step(&app), Some("new-task"), "the first step has nothing before it");

    click(&mut app, "tour");
    assert!(app.view.tour.active.is_none());
    assert_eq!(record(&app, TourId::Agents), Some((1, false)));
    assert!(!has(&frame(&mut app), "popover_surface:tour"));
    assert!(!app.sync_tour(), "a tour closed on the way is not offered again");

    click(&mut app, "tour_start:agents");
    for expected in TourId::Agents.steps() {
        frame(&mut app);
        assert_eq!(step(&app), Some(expected.id));
        click(&mut app, "tour:next");
    }
    assert!(app.view.tour.active.is_none());
    assert_eq!(record(&app, TourId::Agents), Some((1, true)));

    // Seen again and skipped: it stays completed.
    click(&mut app, "tour_start:agents");
    click(&mut app, "tour:next");
    click(&mut app, "tour:skip");
    assert!(app.view.tour.active.is_none());
    assert_eq!(record(&app, TourId::Agents), Some((1, true)));
    // Stray actions of a tour that is over change nothing.
    click(&mut app, "tour:next");
    click(&mut app, "tour:back");
    assert!(app.view.tour.active.is_none());
}

#[test]
fn a_tour_waits_for_a_pending_answer_and_steps_aside_when_one_arrives() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), WIDE);
    let permission = PermissionView {
        request_id: "request".into(),
        tool_name: "apply_patch".into(),
        arguments: "{}".into(),
    };
    app.view.pending_permissions.push(permission.clone());
    click(&mut app, "tour_start:agents");
    assert!(app.view.tour.active.is_none());
    assert!(app.view.settings_notice.is_some());

    app.view.pending_permissions.clear();
    click(&mut app, "tour_start:agents");
    click(&mut app, "tour:next");
    assert!(has(&frame(&mut app), "popover_surface:tour"));

    // A request for permission arrives: the window is the user's again.
    app.view.pending_permissions.push(permission);
    assert!(!app.tour_showing());
    assert!(!has(&frame(&mut app), "popover_surface:tour"));
    assert_eq!(step(&app), Some("tasks"), "the tour keeps its place");
    app.view.pending_permissions.clear();
    assert!(has(&frame(&mut app), "popover_surface:tour"));

    // So does a dialog.
    app.view.sessions.push(SessionView {
        id: "task".into(),
        title: "Tarea".into(),
        project: app.view.workspace.clone(),
        backend: "native".into(),
        pinned: false,
        archived: false,
        unread: false,
        native_automation: None,
    });
    app.view.pending_archive_session_id = Some("task".into());
    assert!(crate::view::dialog_open(&app.view, WIDE.width));
    assert!(!app.tour_showing());
    click(&mut app, "tour_start:agents");
    assert_eq!(step(&app), Some("tasks"), "nor does one start over a dialog");
}

#[test]
fn a_tour_ends_when_the_other_window_mode_is_shown() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), WIDE);
    click(&mut app, "tour_start:agents");
    click(&mut app, "tour:next");
    app.view.ide.mode = WindowMode::Ide;
    assert!(!app.tour_showing());
    assert!(app.sync_tour());
    assert!(app.view.tour.active.is_none());
    assert_eq!(record(&app, TourId::Agents), Some((1, false)));
}

#[test]
fn starting_from_settings_or_the_other_mode_shows_the_tour_in_its_own_window() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), WIDE);
    app.view.show_settings = true;
    app.view.github_account_menu_open = true;
    click(&mut app, "tour_start:ide");
    assert_eq!(app.view.ide.mode, WindowMode::Ide);
    assert!(!app.view.show_settings && !app.view.github_account_menu_open);
    assert_eq!(step(&app), Some("activity"));
    assert!(has(&frame(&mut app), "popover_surface:tour"));

    // The IDE's "Ver" menu starts it too.
    click(&mut app, "tour");
    let entry = format!(
        "ide_menu_action:{}",
        serde_json::to_string(&(None::<(u64, u64)>, "tour_start:ide")).unwrap()
    );
    app.view.ide.menu_open = Some("view".into());
    assert!(has(&frame(&mut app), &entry), "the entry is in sight in the menu");
    click(&mut app, &entry);
    assert!(app.view.ide.menu_open.is_none());
    assert_eq!(step(&app), Some("activity"));
}

#[test]
fn a_step_opens_the_panel_it_points_into_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), WIDE);
    click(&mut app, "toggle_sidebar");
    assert!(app.view.sidebar_collapsed);
    click(&mut app, "tour_start:agents");
    let shown = frame(&mut app);
    assert!(!app.view.sidebar_collapsed);
    assert!(has(&shown, "new_session_sidebar") && has(&shown, "popover_surface:tour"));
    assert_eq!(step(&app), Some("new-task"));
    click(&mut app, "tour");

    click(&mut app, "tour_start:ide");
    app.view.ide.agent_open = false;
    app.view.ide.explorer_open = false;
    let scope = app.dock_scope();
    assert!(app.dock.active_terminal(&scope).is_none());
    let mut seen = Vec::new();
    for _ in TourId::Ide.steps() {
        if has(&frame(&mut app), "popover_surface:tour") {
            seen.extend(step(&app));
        }
        click(&mut app, "tour:next");
    }
    // No file is open in this window, so there are no tabs to point at.
    assert_eq!(
        seen,
        ["activity", "files", "quick-open", "bottom", "agent", "status", "agents"]
    );
    assert!(app.view.ide.explorer_open && app.view.ide.agent_open && app.view.ide.bottom_open);
    assert_eq!(app.view.ide.bottom_panel, crate::ide_workspace::IdeBottomPanel::Problems);
    assert!(app.dock.active_terminal(&scope).is_none(), "no shell is started for a tour");
    assert_eq!(record(&app, TourId::Ide), Some((1, true)));
}

#[test]
fn a_step_with_no_target_is_skipped_the_way_the_user_was_going() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), NARROW);
    click(&mut app, "tour_start:ide");
    frame(&mut app);
    assert_eq!(step(&app), Some("activity"));
    // Too narrow for the explorer beside the editor: it is not opened as a
    // drawer, and the tour moves on to the next thing it can point at.
    click(&mut app, "tour:next");
    let shown = frame(&mut app);
    assert_eq!(step(&app), Some("quick-open"));
    assert!(has(&shown, "popover_surface:tour") && shown.missing_coach_target().is_none());
    assert!(app.view.ide.compact_panel.is_none());
    click(&mut app, "tour:next");
    frame(&mut app);
    assert_eq!(step(&app), Some("bottom"), "no tabs without an open file");
    click(&mut app, "tour:back");
    frame(&mut app);
    assert_eq!(step(&app), Some("quick-open"));
    click(&mut app, "tour:back");
    frame(&mut app);
    assert_eq!(step(&app), Some("activity"));
}

#[test]
fn a_panel_still_opening_is_waited_for_instead_of_skipped() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_at(root.path(), WIDE);
    app.reduced_motion = false;
    click(&mut app, "tour_start:ide");
    app.view.ide.agent_open = false;
    let size = WIDE;
    app.update_dock_motion(size.width, Instant::now());
    let agent = TourId::Ide.steps().iter().position(|step| step.id == "agent").unwrap();
    app.tour_go(agent, false);
    frame(&mut app);
    assert!(app.view.ide.agent_open);
    assert_eq!(step(&app), Some("agent"));
}
