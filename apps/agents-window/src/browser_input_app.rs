use super::*;
use crate::browser_runtime::{BrowserClipboardOperation, BrowserCommand};
use std::collections::VecDeque;

/// A wheel notch on the page, eased like the application's own lists instead
/// of a jump: each frame the page is sent what the motion covered since the
/// last one. A touchpad's own deltas do not come through here; they stay
/// attached to the fingers.
#[derive(Default)]
pub(super) struct WheelGlide {
    /// How far the notches so far ask the page to go, and how much of it
    /// the page was already sent, per axis.
    motion: Option<[(openide_ui::ScalarTransition, f32); 2]>,
}

impl WheelGlide {
    pub(super) fn push(&mut self, dx: f32, dy: f32, now: Instant) {
        let policy = openide_ui::ScrollMotion::default();
        let duration = Duration::from_millis(u64::from(policy.duration_ms));
        let motion = self.motion.get_or_insert_with(|| {
            [(); 2].map(|_| (openide_ui::ScalarTransition::new(0.0, now).easing(policy.easing), 0.0))
        });
        for ((transition, _), delta) in motion.iter_mut().zip([dx, dy]) {
            transition.retarget(transition.target() + delta, now, duration);
        }
    }

    /// What the page scrolls by now. `None` once every notch is spent.
    pub(super) fn step(&mut self, now: Instant) -> Option<(f32, f32)> {
        let motion = self.motion.as_mut()?;
        let mut moved = [0.0; 2];
        let mut active = false;
        for ((transition, sent), moved) in motion.iter_mut().zip(&mut moved) {
            active |= transition.is_active(now);
            *moved = transition.value(now) - *sent;
            *sent += *moved;
        }
        if !active {
            self.motion = None;
        }
        Some((moved[0], moved[1]))
    }
}

const MAX_QUEUED_COMMANDS: usize = 128;
const MAX_QUEUED_BYTES: usize = 256 * 1024;
const MAX_IN_FLIGHT: usize = 4;

#[derive(Default)]
pub(super) struct BrowserInputQueue {
    commands: VecDeque<BrowserCommand>,
    bytes: usize,
}

fn command_bytes(command: &BrowserCommand) -> usize {
    match command {
        BrowserCommand::Type { text } | BrowserCommand::Fill { text, .. } => text.len() + 128,
        BrowserCommand::Navigate { url } => url.len() + 128,
        _ => 128,
    }
}

fn json_text_bytes(text: &str) -> usize {
    text.bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
            0..=31 => 6,
            _ => 1,
        })
        .sum()
}

impl BrowserInputQueue {
    pub(super) fn len(&self) -> usize {
        self.commands.len()
    }
    /// What the footer counts: pointer moves and wheel input are not work
    /// the user waits for.
    fn pending(&self) -> usize {
        self.commands
            .iter()
            .filter(|command| !command.quiet())
            .count()
    }
    fn push(&mut self, command: BrowserCommand) -> Result<(), String> {
        command.validate()?;
        // A move the queue has no room for is skipped, not reported: the next
        // one says where the pointer is.
        if matches!(command, BrowserCommand::PointerMove { .. })
            && !matches!(self.commands.back(), Some(BrowserCommand::PointerMove { .. }))
            && (self.commands.len() >= MAX_QUEUED_COMMANDS
                || self.bytes.saturating_add(command_bytes(&command)) > MAX_QUEUED_BYTES)
        {
            return Ok(());
        }
        if self.bytes.saturating_add(command_bytes(&command)) > MAX_QUEUED_BYTES {
            return Err("La cola de entrada está llena. Esperá y volvé a intentar; esta entrada no se envió.".into());
        }
        match (self.commands.back_mut(), &command) {
            (Some(BrowserCommand::Type { text: previous }), BrowserCommand::Type { text })
                if previous.len() + text.len() <= 16 * 1024
                    && json_text_bytes(previous) + json_text_bytes(text) <= 63 * 1024 =>
            {
                previous.push_str(text);
                self.bytes += text.len();
                return Ok(());
            }
            (
                Some(BrowserCommand::Scroll { dx: x, dy: y, modifiers: held }),
                BrowserCommand::Scroll { dx, dy, modifiers },
            ) if held == modifiers && (*x + dx).abs() <= 100_000.0 && (*y + dy).abs() <= 100_000.0 => {
                *x += dx;
                *y += dy;
                return Ok(());
            }
            // Only where the pointer is now matters: the newest queued move
            // replaces the one before it, never one across a click or a key.
            (
                Some(BrowserCommand::PointerMove { x: last_x, y: last_y, modifiers: held }),
                BrowserCommand::PointerMove { x, y, modifiers },
            ) => {
                (*last_x, *last_y, *held) = (*x, *y, *modifiers);
                return Ok(());
            }
            _ => {}
        }
        if self.commands.len() >= MAX_QUEUED_COMMANDS {
            return Err("La cola de entrada está llena. Esperá y volvé a intentar; esta entrada no se envió.".into());
        }
        self.bytes += command_bytes(&command);
        self.commands.push_back(command);
        Ok(())
    }
    fn pop(&mut self) -> Option<BrowserCommand> {
        let command = self.commands.pop_front()?;
        self.bytes = self.bytes.saturating_sub(command_bytes(&command));
        Some(command)
    }
    fn push_confirmation(&mut self, token: String) {
        // This reserved internal slot follows the corresponding PrepareCut;
        // user input cannot evict it or overtake it.
        let command = BrowserCommand::ConfirmCut { token };
        self.bytes += command_bytes(&command);
        self.commands.push_front(command);
    }
    pub(super) fn navigation_url(&self) -> Option<&str> {
        self.commands
            .iter()
            .rev()
            .find_map(|command| match command {
                BrowserCommand::Navigate { url } => Some(url.as_str()),
                _ => None,
            })
    }
}

impl AgentsWindow {
    pub(super) fn enqueue_browser_input(&mut self, id: u64, command: BrowserCommand) -> bool {
        if !self.browsers.contains_key(&id) {
            self.view.browser.error = Some("El navegador todavía no está disponible".into());
            return false;
        }
        // Passing over the page does not dismiss what an input was told.
        let passing = matches!(command, BrowserCommand::PointerMove { .. });
        match self.browser_inputs.entry(id).or_default().push(command) {
            Ok(()) => {
                if !passing {
                    self.view.browser.input_rejection = None;
                }
                self.drain_browser_inputs(id);
                self.update_browser_queue_count();
                true
            }
            Err(_) if passing => false,
            Err(error) => {
                self.view.status = error.clone();
                self.view.browser.input_rejection = Some(error.clone());
                self.view.browser.error = Some(error);
                false
            }
        }
    }

    pub(super) fn drain_browser_inputs(&mut self, id: u64) {
        loop {
            let requests = self.browser_requests.get(&id);
            let count = requests.map_or(0, Vec::len);
            if count >= MAX_IN_FLIGHT
                || requests.is_some_and(|requests| {
                    requests
                        .iter()
                        .any(|request| request.clipboard_operation().is_some())
                })
            {
                break;
            }
            let Some(queue) = self.browser_inputs.get_mut(&id) else {
                break;
            };
            let clipboard = matches!(
                queue.commands.front(),
                Some(
                    BrowserCommand::CopySelection
                        | BrowserCommand::PrepareCut
                        | BrowserCommand::ConfirmCut { .. }
                )
            );
            if clipboard && count > 0 {
                break;
            }
            let Some(command) = queue.commands.front().cloned() else {
                break;
            };
            // One move or wheel step in flight: those behind it merge in the
            // queue, so the page gets where the pointer is, not where it was.
            if command.quiet()
                && requests.is_some_and(|requests| requests.iter().any(|request| request.quiet()))
            {
                break;
            }
            let Some(runtime) = self.browsers.get(&id) else {
                break;
            };
            let passing = matches!(command, BrowserCommand::PointerMove { .. });
            match runtime.submit_ui(command) {
                Ok(request) => {
                    queue.pop();
                    self.browser_requests.entry(id).or_default().push(request);
                }
                Err(_) if passing => {
                    // A move the page cannot take (it shrank meanwhile) is
                    // dropped; it must not hold the input behind it.
                    queue.pop();
                    continue;
                }
                Err(error) => {
                    // Submit can fail before dispatch (closed scope/full actor
                    // queue). Keep the remaining ordered input visible, do not
                    // replay an operation that might already have run.
                    if self.view.browser.tab_id == Some(id) {
                        self.view.browser.error = Some(error);
                    }
                    break;
                }
            }
            if clipboard {
                break;
            }
        }
        self.browser_inputs.retain(|_, queue| queue.len() > 0);
    }

    pub(super) fn complete_browser_clipboard(
        &mut self,
        id: u64,
        operation: Option<BrowserClipboardOperation>,
        result: &crate::browser_runtime::BrowserSnapshot,
        ctx: &egui::Context,
    ) {
        if !matches!(
            operation,
            Some(BrowserClipboardOperation::Copy | BrowserClipboardOperation::PrepareCut)
        ) {
            return;
        }
        let valid_scope = self.active_browser_id() == Some(id)
            && self.resource_pane_visible()
            && !self.view.show_settings
            && self.view.main_page == MainPage::Workspace
            && self.tree.focused_id() == Some("browser_viewport")
            && self.browser_client_for_current_scope().is_some();
        let Some(clipboard) = result.clipboard.as_ref().filter(|_| valid_scope) else {
            self.view.status =
                "La selección cambió de contexto; no se modificó el portapapeles ni se cortó texto"
                    .into();
            return;
        };
        ctx.copy_text(clipboard.text.clone());
        if operation == Some(BrowserClipboardOperation::PrepareCut) {
            if let Some(token) = &clipboard.cut_token {
                self.browser_inputs
                    .entry(id)
                    .or_default()
                    .push_confirmation(token.clone());
            }
        }
    }

    pub(super) fn update_browser_queue_count(&mut self) {
        self.view.browser.queued_inputs = self.active_browser_id().map_or(0, |id| {
            self.browser_inputs
                .get(&id)
                .map_or(0, BrowserInputQueue::pending)
                + self.browser_requests.get(&id).map_or(0, |requests| {
                    requests.iter().filter(|request| !request.quiet()).count()
                })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_notches_reach_the_page_eased_and_whole() {
        let start = Instant::now();
        let mut glide = WheelGlide::default();
        assert_eq!(glide.step(start), None);
        glide.push(0.0, 80.0, start);
        let mut steps = Vec::new();
        for frame in 1.. {
            let now = start + Duration::from_millis(frame * 8);
            if frame == 5 {
                // A second notch before the first is spent adds to it.
                glide.push(0.0, 80.0, now);
            }
            match glide.step(now) {
                Some((x, y)) => {
                    assert_eq!(x, 0.0);
                    steps.push(y);
                }
                None => break,
            }
        }
        // Many small steps, none backwards, adding up to both notches.
        assert!(steps.len() > 15, "{steps:?}");
        assert!(steps.iter().all(|step| *step >= 0.0 && *step < 40.0), "{steps:?}");
        assert!((steps.iter().sum::<f32>() - 160.0).abs() < 0.01, "{steps:?}");
        // Spent: the next notch starts from nothing.
        glide.push(-30.0, 0.0, start + Duration::from_secs(2));
        assert!(glide.step(start + Duration::from_secs(3)).is_some_and(|(x, y)| (x + 30.0).abs() < 0.01 && y == 0.0));
        assert_eq!(glide.step(start + Duration::from_secs(4)), None);
    }
    #[test]
    fn coalescing_preserves_text_scroll_and_all_command_barriers() {
        let mut queue = BrowserInputQueue::default();
        for _ in 0..20 {
            queue
                .push(BrowserCommand::Type {
                    text: "á🧠".into()
                })
                .unwrap();
        }
        queue
            .push(BrowserCommand::Press {
                key: "ArrowLeft".into(),
            })
            .unwrap();
        queue
            .push(BrowserCommand::Type { text: "fin".into() })
            .unwrap();
        for _ in 0..20 {
            queue
                .push(BrowserCommand::Scroll { dx: 1.5, dy: -2.0, modifiers: 0 })
                .unwrap();
        }
        queue
            .push(BrowserCommand::Click { x: 2.0, y: 3.0 })
            .unwrap();
        queue
            .push(BrowserCommand::Scroll { dx: 4.0, dy: 5.0, modifiers: 0 })
            .unwrap();
        assert_eq!(queue.len(), 6);
        assert!(
            matches!(queue.pop(), Some(BrowserCommand::Type { text }) if text == "á🧠".repeat(20))
        );
        assert!(matches!(queue.pop(), Some(BrowserCommand::Press { .. })));
        assert!(matches!(queue.pop(), Some(BrowserCommand::Type { text }) if text == "fin"));
        assert!(
            matches!(queue.pop(), Some(BrowserCommand::Scroll { dx, dy, .. }) if dx == 30.0 && dy == -40.0)
        );
    }
    #[test]
    fn only_the_newest_pointer_move_waits_and_never_across_a_press_or_a_key() {
        let mut queue = BrowserInputQueue::default();
        let moved = |x: f64| BrowserCommand::PointerMove { x, y: x, modifiers: 0 };
        let (button, clicks) = (openide_browser::MouseButton::Left, 1);
        for command in [
            moved(1.0),
            moved(2.0),
            BrowserCommand::PointerDown { x: 2.0, y: 2.0, button, clicks, modifiers: 0 },
            moved(3.0),
            moved(4.0),
            BrowserCommand::PointerUp { x: 4.0, y: 4.0, button, clicks, modifiers: 0 },
            moved(5.0),
            BrowserCommand::Press { key: "Tab".into() },
            moved(6.0),
            moved(7.0),
        ] {
            queue.push(command).unwrap();
        }
        // Moves and wheel steps are not work the footer counts.
        assert_eq!((queue.len(), queue.pending()), (7, 3));
        let mut order = Vec::new();
        while let Some(command) = queue.pop() {
            order.push(match command {
                BrowserCommand::PointerMove { x, .. } => format!("move {x}"),
                BrowserCommand::PointerDown { .. } => "down".into(),
                BrowserCommand::PointerUp { .. } => "up".into(),
                _ => "key".into(),
            });
        }
        assert_eq!(order, ["move 2", "down", "move 4", "up", "move 5", "key", "move 7"]);
        assert_eq!(queue.bytes, 0);
    }
    #[test]
    fn a_full_queue_skips_a_pointer_move_without_reporting_it_and_still_rejects_input() {
        let mut queue = BrowserInputQueue::default();
        for _ in 0..MAX_QUEUED_COMMANDS {
            queue.push(BrowserCommand::Press { key: "ArrowLeft".into() }).unwrap();
        }
        assert!(queue.push(BrowserCommand::PointerMove { x: 1.0, y: 1.0, modifiers: 0 }).is_ok());
        assert_eq!(queue.len(), MAX_QUEUED_COMMANDS);
        // A press is input the user must be told was not sent.
        let (button, clicks) = (openide_browser::MouseButton::Left, 1);
        assert!(queue.push(BrowserCommand::PointerDown { x: 1.0, y: 1.0, button, clicks, modifiers: 0 }).is_err());
    }
    #[test]
    fn full_queue_rejects_before_mutating_accepted_input() {
        let mut queue = BrowserInputQueue::default();
        for _ in 0..MAX_QUEUED_COMMANDS {
            queue
                .push(BrowserCommand::Press {
                    key: "ArrowLeft".into(),
                })
                .unwrap();
        }
        assert!(queue
            .push(BrowserCommand::Type {
                text: "rechazado".into()
            })
            .is_err());
        assert_eq!(queue.len(), MAX_QUEUED_COMMANDS);
        while let Some(command) = queue.pop() {
            assert!(matches!(command, BrowserCommand::Press { .. }));
        }
        assert_eq!(queue.bytes, 0);
    }
}
