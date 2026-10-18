#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod agent_cli_definitions;
mod cli_profile_settings;
mod agent_cursor;
mod agent_tool_settings;
mod review_model;
mod turn_check;
mod login_shell_path;
mod compat_hook_settings;
mod debug_config;
mod debug_ui;
mod app;
mod approval_ui;
mod i18n;
mod theme_mode;
mod appearance_preferences;
mod appearance_preview;
mod custom_theme;
mod appearance_ui;
mod custom_theme_ui;
mod theme_file_dialog;
mod notifications;
mod model_switch;
mod pause;
mod notifications_ui;
mod tour;
mod tour_ui;
mod automation_view;
mod automations;
mod browser_mcp;
mod browser_runtime;
mod browser_ui;
mod chat_diff_ui;
mod child_process;
mod claude_limits;
#[cfg(test)]
mod code_editor_component_visual_tests;
mod code_editor_files;
mod code_editor_ui;
mod selection_chat;
mod document_preview;
mod document_preview_ui;
mod ide_status_ui;
#[cfg(test)]
mod document_preview_visual_tests;
#[cfg(test)]
mod code_line_alignment_visual_tests;
mod code_snapshot_ui;
mod command_center;
#[cfg(test)]
mod command_center_visual_tests;
mod composer_attachments;
mod file_drop;
mod chime;
mod cli_status;
mod checkpoints;
mod composer_context_ui;
mod composer_pending;
mod composer_permissions_ui;
mod composer_selection;
mod composer_suggestions;
mod composer_ui;
#[cfg(test)]
mod context_visual_tests;
mod conversation_activity;
mod conversation_source_ui;
mod detail_worker;
mod dock;
mod editor_preferences;
mod editor_workbench_ui;
mod editor_review_ui;
mod editor_review_projection;
mod environment_git;
mod environment_git_ui;
mod environment_runtime;
mod environment_selectors_ui;
mod environment_ui;
mod exec_policy_settings;
mod external_agent_settings;
mod file_activity_preview;
mod file_activity_ui;
#[cfg(test)]
mod file_icon_integration_tests;
mod file_watch;
mod file_ops;
mod files;
mod folder_dialog;
mod fonts;
mod acp_run;
mod cli_tasks;
mod graph_layout;
mod graph_ui;
mod graph_view;
mod git_actions;
mod git_branches;
mod git_decorations;
mod git_review;
mod github;
mod github_account;
mod github_account_menu_ui;
mod github_account_ui;
mod github_contributions;
mod browser_sites;
mod browser_settings_ui;
mod subagent_settings;
mod commit_message;
mod mcp_import;
mod mcp_discovery;
mod mcp_presentation;
mod mcp_tool_selection;
mod brand_icons;
mod github_worker;
mod home;
mod ide_workspace;
mod image_runtime;
mod ide_editor_empty;
mod ide_search;
mod keymap;
mod message_actions;
mod message_edit;
mod marketplace;
mod marketplace_view;
mod plugins;
mod plugins_ui;
mod mcp_inspection;
mod mcp_oauth_controller;
#[allow(dead_code)] // Connected to the app in the MCP UI integration slice.
mod mcp_runtime;
#[allow(dead_code)] // Settings are wired into the app in the MCP UI integration slice.
mod mcp_settings;
mod model_catalog;
mod model_metadata;
mod native_automation;
mod native_automation_result;
mod native_command_control_ui;
mod native_command_history_ui;
mod native_command_output_ui;
mod native_command_sessions;
mod native_context_ui;
mod native_goal_ui;
mod native_hook_settings;
mod native_instruction_ui;
mod native_provider;
mod native_skills_settings;
mod navigation;
mod project_actions;
mod project_actions_ui;
mod provider_connections;
mod icons;
mod provider_icons;
mod provider_settings;
mod provider_usage;
mod provider_usage_ui;
mod render;
mod resume_worker;
mod review_worker;
mod chat_links;
mod preview;
mod rich_text;
mod search_activity_ui;
mod search_worker;
mod session_changes;
mod session_changes_editor;
mod session_changes_external;
mod session_changes_ui;
mod session_groups;
mod session_surface;
mod session_tool_diff;
mod settings_parts;
mod settings_preferences;
mod settings_ui;
mod stream_presentation;
mod subagent_dispatch_worker;
mod subagent_recovery_worker;
mod terminal;
mod terminal_ui;
mod terminal_dock_ui;
mod dock_tab_ui;
mod sidebar_chrome;
mod files_panel_ui;
mod theme;
mod mode_proposal_ui;
mod question_ui;
mod tool_card_skeleton;
mod tool_presentation;
mod tool_result_media;
mod transcript;
mod ui_sizing;
mod view;
mod voice;

use app::AgentsWindow;
use eframe::egui;

fn main() -> eframe::Result {
    // The embedded Chromium starts its helper processes as this executable.
    // Such a process is not the application either, and any other start
    // goes on without CEF being called.
    #[cfg(target_os = "linux")]
    if let Some(code) = openide_cef::run_child_process() {
        std::process::exit(code);
    }
    // An external MCP client runs this executable as its proxy to one
    // browser context. That process is not the application: it opens no
    // window and reads no configuration.
    if std::env::args_os().nth(1).is_some_and(|argument| argument == browser_mcp::FLAG) {
        std::process::exit(browser_mcp::main());
    }
    // Checks, without a window, that the embedded Chromium runs here.
    if std::env::args_os().nth(1).is_some_and(|argument| argument == "--cef-probe") {
        std::process::exit(browser_runtime::cef_probe());
    }
    if let Some(code) = openide_harness::run_elevated_command_helper_from_env() {
        std::process::exit(code);
    }
    #[cfg(feature = "editor-e2e")]
    let editor_fixture = std::env::args_os().skip(1).collect::<Vec<_>>();
    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 620.0])
            .with_min_inner_size([520.0, 320.0])
            .with_maximized(true),
        ..Default::default()
    };
    // The window is drawn by the graphics the display hangs from: on a laptop
    // with two, the integrated one. Drawn on the discrete card every frame is
    // copied across, and at 120 Hz one in five was lost once that card
    // throttled down (measured on an AMD + NVIDIA laptop); it also keeps the
    // discrete card awake. `WGPU_POWER_PREF=high` chooses it back.
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.power_preference = eframe::wgpu::PowerPreference::from_env()
            .unwrap_or(eframe::wgpu::PowerPreference::LowPower);
    }
    eframe::run_native(
        "openADE",
        options,
        Box::new(|creation| {
            fonts::install(&creation.egui_ctx);
            #[cfg(feature = "editor-e2e")]
            if editor_fixture
                .first()
                .is_some_and(|arg| arg == "--editor-e2e")
            {
                let root = editor_fixture
                    .get(1)
                    .expect("editor fixture root is required");
                return Ok(Box::new(AgentsWindow::editor_e2e(std::path::Path::new(
                    root,
                ))));
            }
            #[cfg(feature = "editor-e2e")]
            if editor_fixture
                .first()
                .is_some_and(|arg| arg == "--browser-e2e")
            {
                let root = editor_fixture
                    .get(1)
                    .expect("browser fixture root is required");
                return Ok(Box::new(AgentsWindow::browser_e2e(std::path::Path::new(
                    root,
                ))));
            }
            let mut app = AgentsWindow::new();
            app.file_drops = file_drop::FileDrops::attach(creation);
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod context_menu_label_tests;
