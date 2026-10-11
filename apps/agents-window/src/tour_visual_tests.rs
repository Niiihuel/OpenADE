//! Every step of both tours over the real window fixtures: a control that
//! is renamed or removed fails here, not in front of a user.
use super::*;
use crate::ide_workspace::WindowMode;
use crate::tour::{ActiveTour, TourId};
use openide_ui::{Frame, Rect, Size, UiTree};

const WIDE: Size = Size::new(1440.0, 900.0);
/// The narrowest window the application allows, at a usual height.
const NARROW: Size = Size::new(520.0, 680.0);

fn fixture(tour: TourId) -> ViewState {
    match tour {
        TourId::Agents => agent_chrome_visual_tests::fixture(),
        TourId::Ide => ide_visual_tests::fixture(),
    }
}

fn on_step(mut state: ViewState, tour: TourId, step: usize) -> ViewState {
    state.tour.active = Some(ActiveTour {
        tour,
        step,
        backwards: false,
        ensured: true,
    });
    state
}

fn layout(state: &ViewState, size: Size) -> Frame {
    let mut tree = UiTree::new(build_window_view(state, size.width, size.height));
    tree.set_reduced_motion(true);
    tree.layout(size)
}

fn region(frame: &Frame, id: &str) -> Option<Rect> {
    frame.regions.iter().find(|region| region.id == id).map(|region| region.rect)
}

fn bubble(frame: &Frame) -> Option<Rect> {
    region(frame, "popover_surface:tour")
}

#[test]
fn every_step_points_at_controls_that_exist_in_its_window() {
    for tour in TourId::ALL {
        for (index, step) in tour.steps().iter().enumerate() {
            let frame = layout(&on_step(fixture(tour), tour, index), WIDE);
            assert_eq!(frame.missing_coach_target(), None, "{}", step.id);
            for target in step.targets {
                // The home names its project selector differently from a task.
                if *target == "composer_project_toggle" {
                    continue;
                }
                assert!(region(&frame, target).is_some(), "{}: no control {target}", step.id);
            }
            let bubble = bubble(&frame).unwrap_or_else(|| panic!("{}: no bubble", step.id));
            assert!(
                bubble.x >= 0.0
                    && bubble.y >= 0.0
                    && bubble.x + bubble.width <= WIDE.width
                    && bubble.y + bubble.height <= WIDE.height,
                "{}: {bubble:?}",
                step.id
            );
            for id in ["tour:close", "tour:next", "tour:skip"] {
                assert_eq!(
                    region(&frame, id).is_some(),
                    id != "tour:skip" || index + 1 < tour.steps().len(),
                    "{}: {id}",
                    step.id
                );
            }
            assert_eq!(region(&frame, "tour:back").is_some(), index > 0, "{}", step.id);
        }
    }
    // In a task the same selector is the composer's own.
    let mut task = agent_chrome_visual_tests::fixture();
    task.selected_session = Some("workspace-review".into());
    let step = TourId::Agents.steps().iter().position(|step| step.id == "environment").unwrap();
    let frame = layout(&on_step(task, TourId::Agents, step), WIDE);
    assert!(region(&frame, "composer_project_toggle").is_some());
    assert_eq!(frame.missing_coach_target(), None);
}

#[test]
fn the_narrowest_window_keeps_every_bubble_whole_and_misses_only_closed_panels() {
    for (tour, missing) in [(TourId::Agents, &[][..]), (TourId::Ide, &["files", "agent"][..])] {
        for (index, step) in tour.steps().iter().enumerate() {
            let frame = layout(&on_step(fixture(tour), tour, index), NARROW);
            assert_eq!(
                frame.missing_coach_target().is_some(),
                missing.contains(&step.id),
                "{}",
                step.id
            );
            let Some(bubble) = bubble(&frame) else {
                continue;
            };
            assert!(
                bubble.x >= 0.0 && bubble.x + bubble.width <= NARROW.width,
                "{}: {bubble:?}",
                step.id
            );
            let close = region(&frame, "tour:close").unwrap();
            assert!(close.y >= 0.0 && close.y + close.height <= NARROW.height, "{}", step.id);
        }
    }
}

#[test]
fn the_tour_steps_aside_for_a_dialog_a_permission_the_palette_and_the_other_mode() {
    let shown = |state: &ViewState| bubble(&layout(state, WIDE)).is_some();
    let base = || on_step(agent_chrome_visual_tests::fixture(), TourId::Agents, 0);
    assert!(shown(&base()));
    assert!(!dialog_open(&base(), WIDE.width));

    let mut state = base();
    state.pending_archive_session_id = Some("workspace-review".into());
    assert!(dialog_open(&state, WIDE.width));
    assert!(!shown(&state));

    let mut state = base();
    state.pending_permissions.push(PermissionView {
        request_id: "request".into(),
        tool_name: "apply_patch".into(),
        arguments: "{}".into(),
    });
    assert!(!shown(&state));

    let mut state = base();
    state.show_command_center = true;
    assert!(!shown(&state));

    let mut state = base();
    state.ide.mode = WindowMode::Ide;
    assert!(!shown(&state));
}

#[test]
fn the_invitation_belongs_to_its_window_mode_and_leaves_when_a_tour_starts() {
    let offers = |state: &ViewState| {
        let frame = layout(state, WIDE);
        (
            region(&frame, "tour_invite_decline").is_some(),
            region(&frame, "tour_start:agents").is_some(),
        )
    };
    let mut state = agent_chrome_visual_tests::fixture();
    assert_eq!(offers(&state), (false, false));
    state.tour.invites = vec![TourId::Agents];
    assert_eq!(offers(&state), (true, true));
    // The card stays out of the way: the composer is still reachable.
    let frame = layout(&state, WIDE);
    let card = region(&frame, "tour_invite_decline").unwrap();
    assert!(card.y > region(&frame, "run_toggle").unwrap().y);
    state.ide.mode = WindowMode::Ide;
    assert_eq!(offers(&state), (false, false));
    state.ide.mode = WindowMode::Agents;
    let state = on_step(state, TourId::Agents, 0);
    assert_eq!(offers(&state), (false, false));
}

#[test]
fn settings_offer_both_tours_and_a_reset() {
    let mut state = agent_chrome_visual_tests::fixture();
    state.show_settings = true;
    state.settings_page = SettingsPage::General;
    for size in [WIDE, NARROW] {
        let mut tree = UiTree::new(build_window_view(&state, size.width, size.height));
        tree.set_reduced_motion(true);
        tree.layout(size);
        tree.reveal("tour_reset");
        let frame = tree.layout(size);
        for id in ["tour_start:agents", "tour_start:ide", "tour_reset"] {
            assert!(region(&frame, id).is_some(), "{id} at {size:?}");
        }
    }
}

#[test]
fn the_account_menu_starts_the_agents_tour() {
    let mut state = agent_chrome_visual_tests::fixture();
    state.github_account_menu_open = true;
    let frame = layout(&state, WIDE);
    let entry = region(&frame, "tour_start:agents").expect("an entry in the account menu");
    assert!(entry.y >= 0.0 && entry.y + entry.height <= WIDE.height);
}

#[test]
#[ignore = "exports every step of both tours, the invitation and the Settings rows with native fonts"]
fn export_tour_visual_review() {
    let directory = std::env::var("OPENIDE_TOUR_VISUAL_DIR")
        .unwrap_or_else(|_| "/tmp/openade-tour-review".into());
    let export = |state: &ViewState, size: Size, name: &str, reveal: Option<&str>| {
        let (context, mut tree) =
            ide_visual_tests::native_tree(build_window_view(state, size.width, size.height));
        let mut frame = tree.layout(size);
        if let Some(id) = reveal {
            tree.reveal(id);
            frame = tree.layout(size);
        }
        if state.tour.active.is_none() || frame.missing_coach_target().is_none() {
            ide_visual_tests::export_to(&directory, &frame, size, name);
        }
        context.end_pass().textures_delta.clear();
    };
    for size in [WIDE, NARROW] {
        for tour in TourId::ALL {
            for (index, step) in tour.steps().iter().enumerate() {
                let name = format!("{}-{}-{}", tour.key(), index + 1, step.id);
                export(&on_step(fixture(tour), tour, index), size, &name, None);
            }
            let mut state = fixture(tour);
            state.tour.invites = vec![tour];
            export(&state, size, &format!("{}-invitation", tour.key()), None);
        }
        let mut state = agent_chrome_visual_tests::fixture();
        state.show_settings = true;
        state.settings_page = SettingsPage::General;
        export(&state, size, "settings-recorridos", Some("tour_reset"));
    }
}
