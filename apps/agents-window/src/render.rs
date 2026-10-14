//! Painting and input come from limn's egui backend.

use eframe::egui;

pub(crate) use limn_egui::{events, normalize_keyboard_input};

/// Paints a frame in the theme in use: as it was laid out when that is the
/// dark one it was designed in, and with its colors carried over otherwise.
pub(crate) fn paint(ui: &egui::Ui, frame: &openide_ui::Frame) {
    paint_at(ui, frame, ui.max_rect().min);
}

pub(crate) fn paint_at(ui: &egui::Ui, frame: &openide_ui::Frame, origin: egui::Pos2) {
    if crate::theme_mode::light() {
        let mut lit = frame.clone();
        crate::theme_mode::recolor(&mut lit);
        limn_egui::paint_at(ui, &lit, origin);
    } else {
        limn_egui::paint_at(ui, frame, origin);
    }
}
