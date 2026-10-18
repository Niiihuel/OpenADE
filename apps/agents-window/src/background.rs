//! The window's work that does not draw: turns, their notices and the remote.
//!
//! On Wayland winit holds a redraw until the compositor calls the window back
//! after its last frame (egui issue 8326), and a compositor does not call back
//! a window on another workspace or behind a screen that is off. `ui` alone
//! would then stop every turn's events from being read. The event loop still
//! wakes for timers and events, so the window runs through its own loop and
//! each wake looks at that work.

use crate::app::AgentsWindow;
use eframe::{egui, UserEvent};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};

/// How often background work is looked at while the window draws nothing.
pub(crate) const TICK: Duration = Duration::from_millis(200);

/// Wakes the window's loop from another thread without asking for a frame.
#[derive(Clone)]
pub(crate) struct Waker(Arc<Mutex<EventLoopProxy<UserEvent>>>);

impl Waker {
    pub(crate) fn wake(&self) {
        // eframe ignores a repaint request from a pass long gone, so this
        // only runs the loop, and with it `AgentsWindow::background_tick`.
        let proxy = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let _ = proxy.send_event(UserEvent::RequestRepaint {
            viewport_id: egui::ViewportId::ROOT,
            when: Instant::now(),
            cumulative_pass_nr: u64::MAX / 2,
        });
    }
}

/// What the window keeps for its background ticks.
pub(crate) struct Background {
    pub(crate) last_ui_pass: Instant,
    /// The window has the keyboard: the person is at the PC. Kept from the
    /// window's own events, which arrive while it draws nothing too.
    pub(crate) focused: bool,
    pub(crate) ctx: Option<egui::Context>,
    pub(crate) waker: Option<Waker>,
}

impl Default for Background {
    fn default() -> Self {
        Self { last_ui_pass: Instant::now(), focused: true, ctx: None, waker: None }
    }
}

/// eframe owns its app; the loop below needs the same window between frames.
struct Shared(Rc<RefCell<AgentsWindow>>);

impl eframe::App for Shared {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut *self.0.borrow_mut(), ui, frame);
    }

    fn on_exit(&mut self) {
        eframe::App::on_exit(&mut *self.0.borrow_mut());
    }
}

struct Loop<'a> {
    eframe: eframe::EframeWinitApplication<'a>,
    window: Rc<RefCell<Option<Rc<RefCell<AgentsWindow>>>>>,
}

impl Loop<'_> {
    /// Whether the window has background work that needs further ticks.
    fn tick(&mut self) -> bool {
        let window = self.window.borrow();
        let Some(window) = window.as_ref() else { return false };
        // A frame being drawn holds it; that frame does the same work.
        window.try_borrow_mut().is_ok_and(|mut window| window.background_tick())
    }
}

impl ApplicationHandler<UserEvent> for Loop<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.resumed(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        if let winit::event::WindowEvent::Focused(focused) = &event {
            if let Some(Ok(mut window)) = self.window.borrow().as_ref().map(|window| window.try_borrow_mut()) {
                window.background.focused = *focused;
            }
        }
        self.eframe.window_event(event_loop, window_id, event);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.eframe.new_events(event_loop, cause);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.eframe.user_event(event_loop, event);
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        self.eframe.device_event(event_loop, device_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.about_to_wait(event_loop);
        if !self.tick() {
            return;
        }
        let next = Instant::now() + TICK;
        match event_loop.control_flow() {
            ControlFlow::Poll => {}
            ControlFlow::WaitUntil(at) if at <= next => {}
            _ => event_loop.set_control_flow(ControlFlow::WaitUntil(next)),
        }
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.memory_warning(event_loop);
    }
}

/// `eframe::run_native` for a window whose background work goes on while
/// it is hidden.
pub(crate) fn run(
    options: eframe::NativeOptions,
    create: impl FnOnce(&eframe::CreationContext<'_>) -> AgentsWindow,
) -> eframe::Result {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let waker = Waker(Arc::new(Mutex::new(event_loop.create_proxy())));
    let window: Rc<RefCell<Option<Rc<RefCell<AgentsWindow>>>>> = Rc::default();
    let slot = window.clone();
    let eframe = eframe::create_native(
        "openADE",
        options,
        Box::new(move |creation| {
            let mut app = create(creation);
            app.background.waker = Some(waker);
            let app = Rc::new(RefCell::new(app));
            *slot.borrow_mut() = Some(app.clone());
            Ok(Box::new(Shared(app)))
        }),
        &event_loop,
    );
    let mut handler = Loop { eframe, window };
    event_loop.run_app(&mut handler)?;
    Ok(())
}
