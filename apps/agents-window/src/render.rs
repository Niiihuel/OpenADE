//! Painting and input come from limn's egui backend.

use eframe::egui;

pub(crate) use limn_egui::{events, normalize_keyboard_input};

/// Paints native colors in the selected theme and tint; layout stays shared.
pub(crate) fn paint(ui: &egui::Ui, frame: &openide_ui::Frame) {
    paint_at(ui, frame, ui.max_rect().min);
}

pub(crate) fn paint_at(ui: &egui::Ui, frame: &openide_ui::Frame, origin: egui::Pos2) {
    if crate::theme_mode::needs_transform() {
        let mut lit = frame.clone();
        crate::theme_mode::transform(&mut lit);
        limn_egui::paint_at(ui, &lit, origin);
    } else {
        limn_egui::paint_at(ui, frame, origin);
    }
}
