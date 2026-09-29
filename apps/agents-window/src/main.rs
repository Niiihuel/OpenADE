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
mod exec_policy_settings;
mod files;
mod git_review;
mod github;
mod github_account;
mod github_worker;
mod home;
mod marketplace;
mod marketplace_view;
mod mcp_oauth_controller;
#[allow(dead_code)] // Connected to the app in the MCP UI integration slice.
mod mcp_runtime;
#[allow(dead_code)] // Settings are wired into the app in the MCP UI integration slice.
mod mcp_settings;
mod model_catalog;
mod native_command_sessions;
mod native_hook_settings;
mod native_provider;
mod native_skills_settings;
mod navigation;
mod provider_icons;
mod render;
mod resume_worker;
mod review_worker;
mod rich_text;
mod search_worker;
mod session_groups;
mod session_surface;
mod settings_parts;
mod settings_preferences;
mod settings_ui;
mod subagent_recovery_worker;
mod terminal;
mod theme;
mod transcript;
mod view;
mod voice;

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
