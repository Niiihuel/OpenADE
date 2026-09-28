//! Small, backend-independent transitions for interaction colors and geometry.
//!
//! A transition belongs to a stable widget ID, not to a transient element.

use std::time::{Duration, Instant};

use crate::Color;

/// Durations for a menu's floating surface and item feedback. UiTree keeps
/// both transitions by stable ID and snaps them under reduced motion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuMotion {
    pub open_ms: u16,
    pub item_ms: u16,
}

impl Default for MenuMotion {
    fn default() -> Self {
        Self {
            open_ms: 140,
            item_ms: 100,
        }
    }
}

/// Timing curve shared by layout and component transitions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Easing {
    /// A responsive start and a soft finish for short interface movements.
    #[default]
    OutCubic,
    /// A gentle start and finish for larger panel movements.
    InOutCubic,
}

impl Easing {
    pub fn sample(self, progress: f32) -> f32 {
        let progress = progress.clamp(0.0, 1.0);
        match self {
            Self::OutCubic => 1.0 - (1.0 - progress).powi(3),
            Self::InOutCubic if progress < 0.5 => 4.0 * progress.powi(3),
            Self::InOutCubic => 1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0,
        }
    }
}

/// Retargetable animation of one logical-pixel value or opacity. A host owns
/// this state across rebuilt visual trees and schedules repaint while active.
#[derive(Clone, Copy, Debug)]
pub struct ScalarTransition {
    from: f32,
    target: f32,
    started: Instant,
    duration: Duration,
    easing: Easing,
}

impl ScalarTransition {
    pub fn new(value: f32, now: Instant) -> Self {
        let value = if value.is_finite() { value } else { 0.0 };
        Self {
            from: value,
            target: value,
            started: now,
            duration: Duration::ZERO,
            easing: Easing::default(),
        }
    }

    pub fn easing(mut self, easing: Easing) -> Self {
        self.easing = easing;
        self
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn retarget(&mut self, value: f32, now: Instant, duration: Duration) {
        if !value.is_finite() || value == self.target {
            return;
        }
        self.from = self.value(now);
        self.target = value;
        self.started = now;
        self.duration = duration;
    }

    /// Honors a host's reduced-motion preference without changing the
    /// transition's ownership or target semantics.
    pub fn retarget_with_motion(
        &mut self,
        value: f32,
        now: Instant,
        duration: Duration,
        reduced_motion: bool,
    ) {
        if reduced_motion {
            self.snap_to(value, now);
        } else {
            self.retarget(value, now, duration);
        }
    }

    pub fn snap_to(&mut self, value: f32, now: Instant) {
        if !value.is_finite() {
            return;
        }
        self.from = value;
        self.target = value;
        self.started = now;
        self.duration = Duration::ZERO;
    }

    pub fn value(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return self.target;
        }
        let progress = (now.saturating_duration_since(self.started).as_secs_f32()
            / self.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        self.from + (self.target - self.from) * self.easing.sample(progress)
    }

    pub fn is_active(&self, now: Instant) -> bool {
        !self.duration.is_zero()
            && self.from != self.target
            && now.saturating_duration_since(self.started) < self.duration
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ColorTransition {
    from: Color,
    target: Color,
    started: Instant,
    duration: Duration,
}

impl ColorTransition {
    pub(crate) fn new(color: Color, now: Instant) -> Self {
        Self {
            from: color,
            target: color,
            started: now,
            duration: Duration::ZERO,
        }
    }

    pub(crate) fn retarget(&mut self, color: Color, now: Instant, duration: Duration) {
        if color == self.target || (color.a == 0 && self.target.a == 0) {
            return;
        }
        let mut from = self.color(now);
        let mut target = color;
        // Preserve the visible hue while fading to or from an absent fill.
        if from.a == 0 {
            from.r = target.r;
            from.g = target.g;
            from.b = target.b;
        }
        if target.a == 0 {
            target.r = from.r;
            target.g = from.g;
            target.b = from.b;
        }
        self.from = from;
        self.target = target;
        self.started = now;
        self.duration = duration;
    }

    pub(crate) fn color(&self, now: Instant) -> Color {
        if self.duration.is_zero() {
            return self.target;
        }
        let progress = (now.saturating_duration_since(self.started).as_secs_f32()
            / self.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        // Fast onset, soft finish; the same curve applies when retargeting mid-flight.
        let eased = 1.0 - (1.0 - progress).powi(3);
        Color::rgba(
            channel(self.from.r, self.target.r, eased),
            channel(self.from.g, self.target.g, eased),
            channel(self.from.b, self.target.b, eased),
            channel(self.from.a, self.target.a, eased),
        )
    }

    pub(crate) fn is_active(&self, now: Instant) -> bool {
        !self.duration.is_zero()
            && self.from != self.target
            && now.saturating_duration_since(self.started) < self.duration
    }
}

fn channel(from: u8, to: u8, progress: f32) -> u8 {
    (f32::from(from) + (f32::from(to) - f32::from(from)) * progress).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_retarget_keeps_current_position_without_jump() {
        let start = Instant::now();
        let mut scalar = ScalarTransition::new(0.0, start);
        scalar.retarget(100.0, start, Duration::from_millis(200));
        let halfway = start + Duration::from_millis(100);
        let before = scalar.value(halfway);
        assert!(before > 50.0 && before < 100.0);
        scalar.retarget(20.0, halfway, Duration::from_millis(100));
        assert_eq!(scalar.value(halfway), before);
        assert_eq!(scalar.value(halfway + Duration::from_millis(100)), 20.0);
        assert!(!scalar.is_active(halfway + Duration::from_millis(100)));
    }

    #[test]
    fn scalar_reduced_motion_and_invalid_target_are_safe() {
        let start = Instant::now();
        let mut scalar = ScalarTransition::new(30.0, start).easing(Easing::InOutCubic);
        scalar.retarget_with_motion(300.0, start, Duration::from_millis(160), true);
        assert_eq!(scalar.value(start), 300.0);
        assert!(!scalar.is_active(start));
        scalar.retarget(f32::NAN, start, Duration::from_millis(100));
        assert_eq!(scalar.target(), 300.0);
        assert_eq!(Easing::InOutCubic.sample(0.5), 0.5);
    }

    #[test]
    fn transition_reaches_target_and_can_retarget_midway() {
        let start = Instant::now();
        let mut color = ColorTransition::new(Color::rgb(20, 20, 20), start);
        color.retarget(Color::rgb(80, 80, 80), start, Duration::from_millis(120));
        let halfway = start + Duration::from_millis(60);
        assert!(color.is_active(halfway));
        assert!(color.color(halfway).r > 20);
        assert!(color.color(halfway).r < 80);
        let midway_color = color.color(halfway);
        color.retarget(Color::rgb(30, 30, 30), halfway, Duration::from_millis(120));
        assert_eq!(color.color(halfway), midway_color);
        assert_eq!(
            color.color(halfway + Duration::from_millis(120)),
            Color::rgb(30, 30, 30)
        );
        assert!(!color.is_active(halfway + Duration::from_millis(120)));
    }

    #[test]
    fn absent_fill_fades_without_dark_fringe() {
        let start = Instant::now();
        let mut color = ColorTransition::new(Color::TRANSPARENT, start);
        color.retarget(Color::rgb(100, 120, 140), start, Duration::from_millis(100));
        let intermediate = color.color(start + Duration::from_millis(50));
        assert_eq!(
            (intermediate.r, intermediate.g, intermediate.b),
            (100, 120, 140)
        );
        assert!(intermediate.a > 0 && intermediate.a < 255);

        let opaque_at = start + Duration::from_millis(100);
        color.retarget(Color::TRANSPARENT, opaque_at, Duration::from_millis(100));
        let transparent_at = opaque_at + Duration::from_millis(100);
        assert_eq!(color.color(transparent_at).a, 0);
        color.retarget(
            Color::TRANSPARENT,
            transparent_at,
            Duration::from_millis(100),
        );
        assert!(!color.is_active(transparent_at));
    }
}
