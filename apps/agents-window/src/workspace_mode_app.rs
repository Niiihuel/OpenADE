//! Native hover/focus reveal for the shared IDE ↔ Agents titlebar action.

use super::*;

const REVEAL_DURATION: Duration = Duration::from_millis(160);
const EXIT_DELAY: Duration = Duration::from_millis(200);
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
pub(super) struct WorkspaceModeMotion {
    reveal: Option<ScalarTransition>,
    left_at: Option<Instant>,
}

impl WorkspaceModeMotion {
    fn sample(
        &mut self,
        engaged: bool,
        now: Instant,
        reduced_motion: bool,
    ) -> (f32, Option<Duration>) {
        let reveal = self
            .reveal
            .get_or_insert_with(|| ScalarTransition::new(0.0, now).easing(Easing::OutCubic));
        let mut delay = None;
        if reduced_motion || engaged {
            self.left_at = None;
            reveal.retarget_with_motion(
                if engaged { 1.0 } else { 0.0 },
                now,
                REVEAL_DURATION,
                reduced_motion,
            );
        } else if reveal.target() > 0.0 {
            let left_at = *self.left_at.get_or_insert(now);
            let elapsed = now.saturating_duration_since(left_at);
            if elapsed >= EXIT_DELAY {
                reveal.retarget(0.0, now, REVEAL_DURATION);
                self.left_at = None;
            } else {
                delay = Some(EXIT_DELAY - elapsed);
            }
        }
        let repaint = if reveal.is_active(now) {
            Some(FRAME_INTERVAL)
        } else {
            delay
        };
        (reveal.value(now), repaint)
    }
}

impl AgentsWindow {
    /// Called before layout and after input so keyboard and pointer feedback
    /// share the same timing, including when a floating menu takes focus.
    pub(super) fn update_workspace_mode_motion(
        &mut self,
        ctx: &egui::Context,
        now: Instant,
    ) -> bool {
        let present = self
            .tree
            .frame()
            .regions
            .iter()
            .any(|region| region.id == "window_mode_toggle");
        let engaged = present
            && !self.view.show_command_center
            && !self.tree.has_open_popover()
            && ctx.input(|input| input.focused)
            && (self.tree.hovered_id() == Some("window_mode_toggle")
                || self.tree.has_keyboard_focus()
                    && self.tree.focused_id() == Some("window_mode_toggle"));
        let (progress, repaint) =
            self.workspace_mode_motion
                .sample(engaged, now, self.reduced_motion);
        if let Some(delay) = repaint {
            ctx.request_repaint_after(delay);
        }
        let changed = self.view.ide.mode_reveal != progress;
        self.view.ide.mode_reveal = progress;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_mode_reveal_delays_exit_and_finishes_without_another_input_event() {
        let start = Instant::now();
        let mut motion = WorkspaceModeMotion::default();
        assert_eq!(motion.sample(true, start, false).0, 0.0);
        let end = start + REVEAL_DURATION;
        assert_eq!(motion.sample(true, end, false), (1.0, None));
        assert_eq!(motion.sample(false, end, false), (1.0, Some(EXIT_DELAY)));
        assert_eq!(motion.sample(false, end + EXIT_DELAY, false).0, 1.0);
        assert_eq!(
            motion.sample(false, end + EXIT_DELAY + REVEAL_DURATION, false),
            (0.0, None)
        );
    }

    #[test]
    fn workspace_mode_reenter_retargets_from_current_position() {
        let start = Instant::now();
        let mut motion = WorkspaceModeMotion::default();
        motion.sample(true, start, false);
        let end = start + REVEAL_DURATION;
        motion.sample(false, end, false);
        motion.sample(false, end + EXIT_DELAY, false);
        let halfway = end + EXIT_DELAY + REVEAL_DURATION / 2;
        let closing = motion.sample(false, halfway, false).0;
        assert!(closing > 0.0 && closing < 1.0);
        assert_eq!(motion.sample(true, halfway, false).0, closing);
        assert_eq!(
            motion.sample(true, halfway + REVEAL_DURATION, false),
            (1.0, None)
        );
    }

    #[test]
    fn workspace_mode_reduced_motion_skips_animation_and_exit_delay() {
        let start = Instant::now();
        let mut motion = WorkspaceModeMotion::default();
        motion.sample(true, start, false);
        let halfway = start + REVEAL_DURATION / 2;
        assert_eq!(motion.sample(true, halfway, true), (1.0, None));
        assert_eq!(motion.sample(false, halfway, true), (0.0, None));
    }
}
