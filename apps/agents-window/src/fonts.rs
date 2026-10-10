//! Explicit UI font weight, shared by egui's measurement and painting paths.

use eframe::egui;

pub fn install(context: &egui::Context) {
    limn_egui::install_fonts(context);
}
