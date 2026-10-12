//! Acting on the page: the agent's element actions, which wait until the
//! element can take them, and the raw pointer and keyboard events the live
//! view forwards from the user.

use crate::driver::{failed, invalid, timed_out, tool, Driver, Outcome, Resolved, Target, MAX_INPUT_TEXT_BYTES, TIMEOUT};
use crate::keys::{self, Key};
use crate::ErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    #[default]
    Left,
    Middle,
    Right,
}

impl MouseButton {
    fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Middle => "middle",
            Self::Right => "right",
        }
    }

    /// Its bit in the protocol's `buttons` field.
    fn bit(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
        }
    }
}

/// What an element must be before an action is sent to it.
#[derive(Clone, Copy, Default)]
struct Wants {
    enabled: bool,
    editable: bool,
    /// Its box did not move between two animation frames.
    stable: bool,
    /// It is what a click at its centre would reach.
    hit: bool,
}

const POINTER: Wants = Wants {
    enabled: true,
    editable: false,
    stable: true,
    hit: true,
};

/// Pauses between attempts while an element is not ready yet.
const RETRY_PAUSES: [u64; 5] = [0, 20, 100, 100, 500];

fn coordinates(x: f64, y: f64) -> Outcome<()> {
    if x.is_finite() && y.is_finite() && (0.0..=1600.0).contains(&x) && (0.0..=1200.0).contains(&y) {
        Ok(())
    } else {
        Err(invalid("Coordenadas fuera del viewport"))
    }
}

fn input_text(text: &str) -> Outcome<()> {
    if text.len() > MAX_INPUT_TEXT_BYTES || text.contains('\0') {
        Err(invalid("Texto de entrada demasiado extenso"))
    } else {
        Ok(())
    }
}

impl Driver {
    // ---- Element actions ----

    /// Clicks one element once it is visible, still, enabled and what the
    /// click would actually reach.
    pub fn click(&mut self, target: &Target) -> Outcome<()> {
        self.run(TIMEOUT, |driver| {
            let resolved = driver.resolve(target)?;
            let point = driver.actionable(&resolved, POINTER)?;
            driver.click_point(point)
        })
    }

    /// Clicks viewport CSS coordinates, as from a fresh screenshot.
    pub fn click_at(&mut self, x: f64, y: f64) -> Outcome<()> {
        coordinates(x, y)?;
        self.run(TIMEOUT, |driver| driver.click_point((x, y)))
    }

    /// Moves the pointer over one element.
    pub fn hover(&mut self, target: &Target) -> Outcome<()> {
        self.run(TIMEOUT, |driver| {
            let resolved = driver.resolve(target)?;
            let wants = Wants {
                enabled: false,
                ..POINTER
            };
            let point = driver.actionable(&resolved, wants)?;
            driver.move_to(point)
        })
    }

    /// Replaces the value of one field.
    pub fn fill(&mut self, target: &Target, text: &str) -> Outcome<()> {
        input_text(text)?;
        self.run(TIMEOUT, |driver| {
            let resolved = driver.resolve(target)?;
            let wants = Wants {
                enabled: true,
                editable: true,
                ..Wants::default()
            };
            driver.actionable(&resolved, wants)?;
            let object = driver.element(&resolved)?;
            let prepared: Value = driver.library(&resolved.session, Some(&object), None, "fill", json!([text]))?;
            if let Some(error) = prepared["error"].as_str() {
                return Err(failed(error));
            }
            if prepared["done"] == true {
                return Ok(());
            }
            // The old content is selected: typing over it or deleting it
            // goes through the same input events as the user's keyboard.
            if text.is_empty() {
                driver.press_chord("Delete")
            } else {
                driver.insert(text)
            }
        })
    }

    /// Inserts text at the focused field.
    pub fn type_text(&mut self, text: &str) -> Outcome<()> {
        input_text(text)?;
        self.run(TIMEOUT, |driver| driver.insert(text))
    }

    /// Presses a key or a chord such as `Enter` or `Control+A`.
    pub fn press(&mut self, keys: &str) -> Outcome<()> {
        if keys.is_empty() || keys.len() > 128 || keys.chars().any(char::is_control) {
            return Err(invalid("Tecla no válida"));
        }
        // Resolve every key before the first one goes down.
        resolve_chord(keys, 0)?;
        self.run(TIMEOUT, |driver| driver.press_chord(keys))
    }

    /// Turns the wheel at the pointer by CSS pixel deltas.
    pub fn scroll(&mut self, dx: f64, dy: f64) -> Outcome<()> {
        if !(dx.is_finite() && dy.is_finite() && dx.abs() <= 100_000.0 && dy.abs() <= 100_000.0) {
            return Err(invalid("Desplazamiento no válido"));
        }
        self.run(TIMEOUT, |driver| driver.wheel_at(driver.pointer, dx, dy))
    }

    /// Chooses options of one `<select>`. Each value matches an option's
    /// value or its label; several need a multiple select.
    pub fn select_options(&mut self, target: &Target, values: &[&str]) -> Outcome<()> {
        let acceptable = |value: &&str| value.len() <= 1024 && !value.chars().any(char::is_control);
        if values.is_empty() || values.len() > 32 || !values.iter().all(acceptable) {
            return Err(invalid("Indicá entre 1 y 32 opciones de hasta 1024 bytes"));
        }
        self.run(TIMEOUT, |driver| {
            let resolved = driver.resolve(target)?;
            let wants = Wants {
                enabled: true,
                ..Wants::default()
            };
            driver.actionable(&resolved, wants)?;
            // The options may still be arriving: ask until they are all there.
            for pause in RETRY_PAUSES.into_iter().chain(std::iter::repeat(500)) {
                std::thread::sleep(Duration::from_millis(pause));
                let object = driver.element(&resolved)?;
                let chosen: Value = driver.library(&resolved.session, Some(&object), None, "select", json!([values]))?;
                if let Some(error) = chosen["error"].as_str() {
                    return Err(failed(error));
                }
                if chosen["missing"] != true {
                    return Ok(());
                }
                if driver.deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(500) {
                    break;
                }
            }
            Err(failed(format!("{} No hay una opción habilitada con ese valor o etiqueta.", timed_out())))
        })
    }

    /// Sets one checkbox, radio button or switch. A control that hides its
    /// real input is clicked where the user would: on its label.
    pub fn set_checked(&mut self, target: &Target, checked: bool) -> Outcome<()> {
        self.run(TIMEOUT, |driver| {
            let resolved = driver.resolve(target)?;
            if driver.checked(&resolved)? == checked {
                return Ok(());
            }
            let object = driver.element(&resolved)?;
            // One look, without waiting: is the input itself what a click reaches?
            let direct: Value = driver.library(&resolved.session, Some(&object), None, "state", json!([state_wants(POINTER)]))?;
            let reachable = !matches!(direct["state"].as_str(), Some("hidden" | "covered" | "offscreen"));
            let stand_in = if reachable {
                None
            } else {
                let function = crate::scripts::LIBRARY;
                let found = driver.invoke(&resolved.session, Some(&object), None, function, json!([{"value": "stand_in"}]), false)?;
                found["objectId"].as_str().map(str::to_owned)
            };
            let point = match stand_in {
                Some(stand_in) => driver.actionable_object(&resolved, &stand_in, POINTER)?,
                None => driver.actionable(&resolved, POINTER)?,
            };
            driver.click_point(point)?;
            // The click happened: what goes wrong from here is an uncertain effect, not a refusal.
            match driver.checked(&resolved) {
                Ok(now) if now == checked => Ok(()),
                Ok(_) => Err(failed("Clicking the checkbox did not change its state")),
                Err(error) if error.kind == ErrorKind::Tool => Err(failed(error.message)),
                Err(error) => Err(error),
            }
        })
    }

    fn checked(&mut self, resolved: &Resolved) -> Outcome<bool> {
        let object = self.element(resolved)?;
        let state: Value = self.library(&resolved.session, Some(&object), None, "checked", json!([]))?;
        match (state["checked"].as_bool(), state["error"].as_str()) {
            (Some(checked), _) => Ok(checked),
            (None, error) => Err(tool(error.unwrap_or("Not a checkbox or radio button"))),
        }
    }

    /// Drags one element and drops it on another: with the pointer, and as an
    /// HTML drag and drop when the page starts one.
    pub fn drag(&mut self, from: &Target, to: &Target) -> Outcome<()> {
        self.run(TIMEOUT, |driver| {
            // Both ends are found before either is touched.
            let source = driver.resolve(from)?;
            let destination = driver.resolve(to)?;
            let session = driver.session.clone();
            let start = driver.actionable(&source, POINTER)?;
            driver.move_to(start)?;
            driver.pointer_event("mousePressed", start, MouseButton::Left, 1)?;
            let dropped = (|| -> Outcome<()> {
                let object = driver.element(&source)?;
                driver.library::<bool>(&source.session, Some(&object), None, "watch_drag", json!([]))?;
                driver.shared.state().drag = None;
                // Chrome hands an HTML drag over instead of running it itself,
                // which would wait for a pointer that is not there.
                driver.call::<Value>(&session, "Input.setInterceptDrags", json!({"enabled": true}))?;
                let dropped = driver.drop_on(&source, &destination);
                let _ = driver.call::<Value>(&session, "Input.setInterceptDrags", json!({"enabled": false}));
                dropped
            })();
            if dropped.is_err() && driver.buttons & MouseButton::Left.bit() != 0 {
                // Never leave the page with a button held down.
                let _ = driver.pointer_event("mouseReleased", driver.pointer, MouseButton::Left, 1);
            }
            dropped
        })
    }

    fn drop_on(&mut self, source: &Resolved, destination: &Resolved) -> Outcome<()> {
        let session = self.session.clone();
        let wants = Wants {
            enabled: false,
            ..POINTER
        };
        let end = self.actionable(destination, wants)?;
        self.move_to(end)?;
        let world = self.world(&source.session, &source.frame)?;
        let started: bool = self.library(&source.session, None, Some(world), "drag_started", json!([]))?;
        // The hand-over is announced by an event that may trail the move.
        let limit = Instant::now() + Duration::from_millis(if started { 2000 } else { 0 });
        let data = self.shared.wait(limit.min(self.deadline), |state| state.drag.take());
        let Some(data) = data else {
            return self.pointer_event("mouseReleased", end, MouseButton::Left, 1);
        };
        for kind in ["dragEnter", "dragOver", "drop"] {
            self.call::<Value>(
                &session,
                "Input.dispatchDragEvent",
                json!({"type": kind, "x": end.0, "y": end.1, "data": data, "modifiers": self.modifiers}),
            )?;
        }
        // The drop ends the gesture: the page never sees the button go up.
        self.buttons &= !MouseButton::Left.bit();
        self.pointer = end;
        Ok(())
    }

    /// Waits until the element can take the action and returns where to
    /// click it, in CSS pixels of the page's viewport.
    fn actionable(&mut self, resolved: &Resolved, wants: Wants) -> Outcome<(f64, f64)> {
        let object = self.element(resolved)?;
        self.actionable_object(resolved, &object, wants)
    }

    /// The same for a handle that lives in `resolved`'s frame.
    fn actionable_object(&mut self, resolved: &Resolved, object: &str, wants: Wants) -> Outcome<(f64, f64)> {
        let mut pauses = RETRY_PAUSES.into_iter().chain(std::iter::repeat(500));
        loop {
            // An element without a box cannot be scrolled to; the state says why.
            let _: Outcome<Value> = self.call(&resolved.session, "DOM.scrollIntoViewIfNeeded", json!({"objectId": object}));
            let state: Value = self.library(&resolved.session, Some(object), None, "state", json!([state_wants(wants)]))?;
            let reason = match state["state"].as_str().unwrap_or("") {
                "ready" => {
                    let origin = self.frame_origin(&resolved.session, &resolved.frame, 0)?;
                    let point = (state["x"].as_f64().unwrap_or(0.0), state["y"].as_f64().unwrap_or(0.0));
                    return Ok((origin.0 + point.0, origin.1 + point.1));
                }
                "detached" => return Err(failed("Element is not attached to the DOM")),
                "hidden" => "el elemento no es visible".to_owned(),
                "disabled" => "el elemento está deshabilitado".to_owned(),
                "readonly" => "el elemento es de sólo lectura".to_owned(),
                "moving" => "el elemento no deja de moverse".to_owned(),
                "offscreen" => "el elemento queda fuera de la vista".to_owned(),
                "covered" => format!(
                    "{} recibe los eventos del puntero en su lugar",
                    state["by"].as_str().filter(|by| !by.is_empty()).unwrap_or("otro elemento")
                ),
                _ => "la página no respondió".to_owned(),
            };
            let pause = Duration::from_millis(pauses.next().unwrap_or(500));
            if self.deadline.saturating_duration_since(Instant::now()) <= pause {
                return Err(failed(format!("{} Todavía {reason}.", timed_out())));
            }
            std::thread::sleep(pause);
        }
    }

    fn click_point(&mut self, point: (f64, f64)) -> Outcome<()> {
        let before = self.navigation_mark();
        self.move_to(point)?;
        self.pointer_event("mousePressed", point, MouseButton::Left, 1)?;
        self.pointer_event("mouseReleased", point, MouseButton::Left, 1)?;
        self.settle_navigation(before);
        Ok(())
    }

    fn navigation_mark(&self) -> (u64, u64, u64) {
        let state = self.shared.state();
        (state.requested, state.commits, state.stopped)
    }

    /// If the action made the page start a navigation, waits until the new
    /// document is committed and parsed, so what is read next is neither the
    /// page being left nor one that has yet to say what it holds.
    fn settle_navigation(&mut self, before: (u64, u64, u64)) {
        // One round trip through the page: its request to navigate, if any,
        // is reported before this answer.
        let session = self.session.clone();
        let _: Outcome<Value> = self.call(&session, "Runtime.evaluate", json!({"expression": "0"}));
        self.shared.wait(self.deadline, |state| {
            let pending = state.requested > before.0 && state.commits == before.1 && state.stopped == before.2;
            let parsing = state.commits > before.1 && state.dom_content_loader != state.loader;
            (!pending && !parsing).then_some(())
        });
    }

    fn insert(&mut self, text: &str) -> Outcome<()> {
        let session = self.session.clone();
        self.call::<Value>(&session, "Input.insertText", json!({"text": text}))?;
        Ok(())
    }

    fn press_chord(&mut self, keys: &str) -> Outcome<()> {
        let held = resolve_chord(keys, self.modifiers)?;
        let before = self.navigation_mark();
        for key in &held {
            self.key_event(key, true)?;
        }
        for key in held.iter().rev() {
            self.key_event(key, false)?;
        }
        self.settle_navigation(before);
        Ok(())
    }

    // ---- Raw input for a live view ----

    /// The modifier keys held (`modifiers::SHIFT | …`) with the pointer and
    /// wheel events that follow, without key events of their own: a host
    /// that learns of them with each pointer event sets them before it and
    /// clears them after.
    pub fn hold_modifiers(&mut self, modifiers: u8) {
        self.modifiers = modifiers & (keys::ALT | keys::CONTROL | keys::META | keys::SHIFT);
    }

    /// Moves the pointer. Buttons already down stay down, so a move between
    /// `pointer_down` and `pointer_up` is a drag.
    pub fn pointer_move(&mut self, x: f64, y: f64) -> Outcome<()> {
        coordinates(x, y)?;
        self.run(TIMEOUT, |driver| driver.move_to((x, y)))
    }

    /// Presses a button. `clicks` is 2 for the second press of a double click.
    pub fn pointer_down(&mut self, x: f64, y: f64, button: MouseButton, clicks: u8) -> Outcome<()> {
        coordinates(x, y)?;
        self.run(TIMEOUT, |driver| driver.pointer_event("mousePressed", (x, y), button, clicks))
    }

    pub fn pointer_up(&mut self, x: f64, y: f64, button: MouseButton, clicks: u8) -> Outcome<()> {
        coordinates(x, y)?;
        self.run(TIMEOUT, |driver| driver.pointer_event("mouseReleased", (x, y), button, clicks))
    }

    /// Turns the wheel at a position by CSS pixel deltas.
    pub fn wheel(&mut self, x: f64, y: f64, dx: f64, dy: f64) -> Outcome<()> {
        coordinates(x, y)?;
        if !(dx.is_finite() && dy.is_finite() && dx.abs() <= 100_000.0 && dy.abs() <= 100_000.0) {
            return Err(invalid("Desplazamiento no válido"));
        }
        self.run(TIMEOUT, |driver| driver.wheel_at((x, y), dx, dy))
    }

    /// Holds one key down. A modifier stays in effect for the keys and
    /// pointer events that follow, until its `key_up`.
    pub fn key_down(&mut self, key: &str) -> Outcome<()> {
        let key = named_key(key, self.modifiers)?;
        self.run(TIMEOUT, |driver| driver.key_event(&key, true))
    }

    pub fn key_up(&mut self, key: &str) -> Outcome<()> {
        let key = named_key(key, self.modifiers)?;
        self.run(TIMEOUT, |driver| driver.key_event(&key, false))
    }

    /// Inserts text at the focus without key events, as an input method does.
    pub fn insert_text(&mut self, text: &str) -> Outcome<()> {
        self.type_text(text)
    }

    /// A pointer move. A button that is down travels with it.
    fn move_to(&mut self, point: (f64, f64)) -> Outcome<()> {
        let session = self.session.clone();
        let held = [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
            .into_iter()
            .find(|button| self.buttons & button.bit() != 0);
        self.call::<Value>(
            &session,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseMoved",
                "x": point.0,
                "y": point.1,
                "button": held.map_or("none", MouseButton::name),
                "buttons": self.buttons,
                "modifiers": self.modifiers,
            }),
        )?;
        self.pointer = point;
        Ok(())
    }

    fn pointer_event(&mut self, kind: &str, point: (f64, f64), button: MouseButton, clicks: u8) -> Outcome<()> {
        if kind == "mousePressed" {
            self.buttons |= button.bit();
        } else {
            self.buttons &= !button.bit();
        }
        let session = self.session.clone();
        self.call::<Value>(
            &session,
            "Input.dispatchMouseEvent",
            json!({
                "type": kind,
                "x": point.0,
                "y": point.1,
                "button": button.name(),
                "buttons": self.buttons,
                "clickCount": clicks.clamp(1, 3),
                "modifiers": self.modifiers,
            }),
        )?;
        self.pointer = point;
        Ok(())
    }

    fn wheel_at(&mut self, point: (f64, f64), dx: f64, dy: f64) -> Outcome<()> {
        let session = self.session.clone();
        self.call::<Value>(
            &session,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseWheel",
                "x": point.0,
                "y": point.1,
                "deltaX": dx,
                "deltaY": dy,
                "buttons": self.buttons,
                "modifiers": self.modifiers,
            }),
        )?;
        self.pointer = point;
        Ok(())
    }

    fn key_event(&mut self, key: &Key, down: bool) -> Outcome<()> {
        if down {
            self.modifiers |= key.modifier;
        } else {
            self.modifiers &= !key.modifier;
        }
        // With Control, Alt or Meta held a key is a shortcut and types nothing.
        let text = if self.modifiers & !keys::SHIFT == 0 { key.text.as_str() } else { "" };
        let kind = match (down, text.is_empty()) {
            (true, false) => "keyDown",
            (true, true) => "rawKeyDown",
            (false, _) => "keyUp",
        };
        let mut params = json!({
            "type": kind,
            "modifiers": self.modifiers,
            "key": key.key,
            "code": key.code,
            "windowsVirtualKeyCode": key.key_code,
            "location": key.location,
        });
        if down && !text.is_empty() {
            params["text"] = Value::from(text);
            params["unmodifiedText"] = Value::from(text);
        }
        let session = self.session.clone();
        self.call::<Value>(&session, "Input.dispatchKeyEvent", params)?;
        Ok(())
    }
}

fn state_wants(wants: Wants) -> Value {
    json!({"enabled": wants.enabled, "editable": wants.editable, "stable": wants.stable, "hit": wants.hit})
}

fn named_key(name: &str, modifiers: u8) -> Outcome<Key> {
    keys::key(name, modifiers & keys::SHIFT != 0).ok_or_else(|| invalid(format!("Tecla desconocida: {}", crate::observer::limited(name, 64))))
}

/// The keys of a chord in the order they go down. A key named by position
/// types its shifted character when Shift is among the ones before it.
fn resolve_chord(keys: &str, mut modifiers: u8) -> Outcome<Vec<Key>> {
    let mut held = Vec::new();
    for name in keys::chord(keys) {
        let key = named_key(name, modifiers)?;
        modifiers |= key.modifier;
        held.push(key);
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_resolves_every_key_before_any_is_pressed() {
        let held = resolve_chord("Control+Shift+KeyA", 0).unwrap();
        let names: Vec<_> = held.iter().map(|key| (key.key.as_str(), key.key_code)).collect();
        assert_eq!(names, [("Control", 17), ("Shift", 16), ("A", 65)], "Shift before a position shifts it");
        assert_eq!(resolve_chord("Control+a", 0).unwrap()[1].key, "a");
        assert_eq!(resolve_chord("Control++", 0).unwrap()[1].code, "Equal");
        for unknown in ["Control+Nope", "Control+", "Ctrl+A", ""] {
            let error = resolve_chord(unknown, 0).unwrap_err();
            assert_eq!(error.kind, crate::ErrorKind::Invalid, "{unknown:?}");
            assert!(error.message.starts_with("Tecla desconocida"));
        }
    }

    #[test]
    fn coordinates_text_and_wants_are_checked_before_the_browser_is_asked() {
        for (x, y) in [(0.0, 0.0), (1600.0, 1200.0), (12.5, 700.25)] {
            assert!(coordinates(x, y).is_ok());
        }
        for (x, y) in [(-0.1, 0.0), (1600.1, 0.0), (0.0, 1200.1), (f64::NAN, 0.0), (0.0, f64::INFINITY)] {
            assert_eq!(coordinates(x, y).unwrap_err().message, "Coordenadas fuera del viewport");
        }
        assert!(input_text(&"é".repeat(MAX_INPUT_TEXT_BYTES / 2)).is_ok());
        assert!(input_text(&"x".repeat(MAX_INPUT_TEXT_BYTES + 1)).is_err());
        assert!(input_text("a\0b").is_err());
        assert_eq!(
            state_wants(POINTER),
            json!({"enabled": true, "editable": false, "stable": true, "hit": true})
        );
    }
}
