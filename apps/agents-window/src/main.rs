#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod agent_tool_settings;
mod app;
mod automation_view;
mod automations;
mod child_process;
mod command_center;
mod composer_selection;
mod composer_ui;
mod detail_worker;
mod dock;
mod files;
mod git_review;
mod github;
mod github_account;
mod github_worker;
mod home;
mod marketplace;
mod marketplace_view;
mod model_catalog;
mod native_provider;
mod navigation;
mod render;
mod review_worker;
mod rich_text;
mod search_worker;
mod session_groups;
mod session_surface;
mod settings_parts;
mod settings_preferences;
mod settings_ui;
mod terminal;
mod theme;
mod transcript;
mod view;

use app::AgentsWindow;
use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 620.0])
            .with_min_inner_size([520.0, 320.0])
            .with_maximized(true),
        ..Default::default()
    };
    eframe::run_native(
        "OpenIDE",
        options,
        Box::new(|_| Ok(Box::new(AgentsWindow::new()))),
    )
}
