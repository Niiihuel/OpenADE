//! Presentation preferences for two views of the same live workspace.
//! Buffers, sessions, PTYs and harness state stay in their existing owners.
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::view::{DockLayout, MainPage, ViewState};

pub(crate) const OUTER_INSET: f32 = 4.0;
pub(crate) const TITLEBAR_HEIGHT: f32 = 32.0;
pub(crate) const STATUSBAR_HEIGHT: f32 = 22.0;
pub(crate) const ACTIVITY_BAR_WIDTH: f32 = 40.0;
pub(crate) const DIVIDER: f32 = 1.0;
/// Match the breathing room between the Agents sidebar and its workspace island.
pub(crate) const SIDEBAR_GAP: f32 = 4.0;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WindowMode {
    #[default]
    Agents,
    Ide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdePanel {
    Explorer,
    Agent,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IdeActivity {
    #[default]
    Files,
    Search,
    Git,
    Debug,
    Extensions,
}

impl IdeActivity {
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "files" => Self::Files,
            "search" => Self::Search,
            "git" => Self::Git,
            "debug" => Self::Debug,
            "extensions" => Self::Extensions,
            _ => return None,
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Search => "search",
            Self::Git => "git",
            Self::Debug => "debug",
            Self::Extensions => "extensions",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Files => "Archivos",
            Self::Search => "Buscar",
            Self::Git => "Control de versiones",
            Self::Debug => "Depuración",
            Self::Extensions => "Complementos",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IdeBottomPanel {
    #[default]
    Terminal,
    Problems,
    Output,
    DebugConsole,
}

#[derive(Clone, Debug)]
pub(crate) struct IdeNewFile {
    pub workspace: String,
    pub path: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct IdeWorkspaceState {
    pub mode: WindowMode,
    pub explorer_open: bool,
    pub agent_open: bool,
    pub explorer_width: f32,
    pub agent_width: f32,
    /// Stable content widths captured by the host for a dock toggle in flight.
    #[serde(skip)]
    pub explorer_content_width: Option<f32>,
    #[serde(skip)]
    pub agent_content_width: Option<f32>,
    pub activity: IdeActivity,
    pub bottom_panel: IdeBottomPanel,
    pub bottom_open: bool,
    #[serde(skip)]
    pub extensions_open: bool,
    #[serde(skip)]
    pub extensions_item: Option<String>,
    #[serde(skip)]
    pub extensions_workspace: Option<String>,
    /// The center tab underneath the catalog; navigation reveals a new target.
    #[serde(skip)]
    pub extensions_editor_tab: Option<u64>,
    #[serde(skip)]
    pub git_actions_open: bool,
    #[serde(skip)]
    pub sessions_open: bool,
    /// Tasks open as tabs of the agent panel, in the order they were opened.
    #[serde(skip)]
    pub agent_tabs: Vec<String>,
    #[serde(skip)]
    pub agent_new_open: bool,
    /// The list of tasks beside the agent panel's content.
    pub agent_list_open: bool,
    /// Reveal of the task list while its own transition is in flight.
    #[serde(skip)]
    pub agent_list_progress: Option<f32>,
    #[serde(skip)]
    pub agent_list_query: String,
    #[serde(skip)]
    pub menu_open: Option<String>,
    #[serde(skip)]
    pub mode_reveal: f32,
    #[serde(skip)]
    pub activity_focus: Option<String>,
    #[serde(skip)]
    pub compact_panel: Option<IdePanel>,
    #[serde(skip)]
    pub new_file: Option<IdeNewFile>,
}

impl Default for IdeWorkspaceState {
    fn default() -> Self {
        Self {
            mode: WindowMode::Agents,
            explorer_open: true,
            agent_open: true,
            explorer_width: 240.0,
            agent_width: 380.0,
            explorer_content_width: None,
            agent_content_width: None,
            activity: IdeActivity::Files,
            bottom_panel: IdeBottomPanel::Terminal,
            bottom_open: false,
            extensions_open: false,
            extensions_item: None,
            extensions_workspace: None,
            extensions_editor_tab: None,
            git_actions_open: false,
            sessions_open: false,
            agent_tabs: Vec::new(),
            agent_new_open: false,
            agent_list_open: false,
            agent_list_progress: None,
            agent_list_query: String::new(),
            menu_open: None,
            mode_reveal: 0.0,
            activity_focus: None,
            compact_panel: None,
            new_file: None,
        }
    }
}

impl IdeWorkspaceState {
    pub fn is_active(&self) -> bool {
        self.mode == WindowMode::Ide
    }

    pub fn load(root: &Path) -> Self {
        let mut value: Self = std::fs::read(root.join("workspace-layout.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        value.explorer_width = valid_width(value.explorer_width, 240.0, 180.0, 420.0);
        value.agent_width = valid_width(value.agent_width, 380.0, 320.0, 720.0);
        value
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        let temporary = root.join(format!(".workspace-layout-{}.tmp", std::process::id()));
        std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, root.join("workspace-layout.json"))
            .map_err(|error| error.to_string())
    }
}

fn valid_width(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

pub(crate) fn active(state: &ViewState) -> bool {
    state.ide.is_active() && !state.show_settings && state.main_page == MainPage::Workspace
}

/// Responsive visibility never overwrites the preferred sizes or open state.
pub(crate) fn layout(state: &ViewState, viewport: f32) -> DockLayout {
    let nominal = nominal_layout(state, viewport);
    let inner = (viewport - OUTER_INSET * 2.0 - ACTIVITY_BAR_WIDTH).max(0.0);
    let left = state
        .left_presentation_width
        .unwrap_or(nominal.left)
        .clamp(0.0, inner);
    let left_extent = if left > 0.0 { left + SIDEBAR_GAP } else { 0.0 };
    let right = state
        .right_presentation_width
        .or(nominal.right)
        .filter(|width| *width > 0.0)
        .map(|width| {
            (width + agent_list_extent(state, viewport))
                .clamp(0.0, (inner - left_extent - DIVIDER).max(0.0))
        });
    DockLayout {
        left,
        right,
        center: (inner - left_extent - right.map_or(0.0, |width| width + DIVIDER)).max(0.0),
        left_split_x: (left > 0.0)
            .then_some(OUTER_INSET + ACTIVITY_BAR_WIDTH + left + SIDEBAR_GAP / 2.0),
        right_split_x: right.map(|width| viewport - OUTER_INSET - width - DIVIDER / 2.0),
    }
}

/// Requested visibility and dimensions, independent of a toggle in flight.
/// Width of the task list column of the agent dock.
pub(crate) const AGENT_LIST_WIDTH: f32 = 220.0;

/// Settled width of the task list: it takes room from the editor, never from
/// the conversation, and gives way when the editor would get too narrow.
pub(crate) fn agent_list_room(state: &ViewState, viewport: f32) -> f32 {
    let nominal = nominal_layout(state, viewport);
    if nominal.right.is_none() {
        return 0.0;
    }
    AGENT_LIST_WIDTH.min((nominal.center - 360.0 - DIVIDER).max(0.0))
}

pub(crate) fn agent_list_shown(state: &ViewState, viewport: f32) -> bool {
    state.ide.agent_list_open && agent_list_room(state, viewport) > 0.0
}

/// Presented width of the task list, which opens with its own transition.
pub(crate) fn agent_list_width(state: &ViewState, viewport: f32) -> f32 {
    let progress = state.ide.agent_list_progress.unwrap_or(
        if agent_list_shown(state, viewport) {
            1.0
        } else {
            0.0
        },
    );
    let room = if state.ide.agent_list_open {
        agent_list_room(state, viewport)
    } else {
        AGENT_LIST_WIDTH
    };
    room * progress.clamp(0.0, 1.0)
}

fn agent_list_extent(state: &ViewState, viewport: f32) -> f32 {
    let width = agent_list_width(state, viewport);
    if width > 0.0 {
        width + DIVIDER
    } else {
        0.0
    }
}

pub(crate) fn nominal_layout(state: &ViewState, viewport: f32) -> DockLayout {
    let inner = (viewport - OUTER_INSET * 2.0 - ACTIVITY_BAR_WIDTH).max(0.0);
    let left_visible = state.ide.explorer_open && inner >= 320.0 + 180.0 + SIDEBAR_GAP;
    let left_min = if left_visible {
        180.0 + SIDEBAR_GAP
    } else {
        0.0
    };
    let right_visible = state.ide.agent_open && inner >= left_min + 360.0 + 320.0 + DIVIDER;
    let right_min = if right_visible { 320.0 + DIVIDER } else { 0.0 };
    let left = if left_visible {
        valid_width(state.ide.explorer_width, 240.0, 180.0, 420.0).min(
            (inner - right_min - if right_visible { 360.0 } else { 320.0 } - SIDEBAR_GAP).max(0.0),
        )
    } else {
        0.0
    };
    let left_extent = if left_visible {
        left + SIDEBAR_GAP
    } else {
        0.0
    };
    let right = right_visible.then(|| {
        valid_width(state.ide.agent_width, 380.0, 320.0, 720.0)
            .min((inner - left_extent - 360.0 - DIVIDER).max(0.0))
    });
    DockLayout {
        left,
        right,
        center: (inner - left_extent - right.map_or(0.0, |width| width + DIVIDER)).max(0.0),
        left_split_x: left_visible
            .then_some(OUTER_INSET + ACTIVITY_BAR_WIDTH + left + SIDEBAR_GAP / 2.0),
        right_split_x: right.map(|width| viewport - OUTER_INSET - width - DIVIDER / 2.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_bar_row_is_limns_bar_under_one_hairline() {
        assert_eq!(STATUSBAR_HEIGHT, openide_ui::StatusBar::HEIGHT + 1.0);
    }

    #[test]
    fn ide_preferences_round_trip_without_transient_history() {
        let root = tempfile::tempdir().unwrap();
        let mut state = IdeWorkspaceState::load(root.path());
        assert_eq!(state.mode, WindowMode::Agents);
        state.mode = WindowMode::Ide;
        state.explorer_width = 276.0;
        state.agent_width = 412.0;
        state.explorer_content_width = Some(276.0);
        state.agent_content_width = Some(412.0);
        state.agent_open = false;
        state.activity = IdeActivity::Git;
        state.bottom_panel = IdeBottomPanel::Problems;
        state.bottom_open = true;
        state.extensions_open = true;
        state.extensions_item = Some("local:review".into());
        state.extensions_workspace = Some("/project".into());
        state.sessions_open = true;
        state.menu_open = Some("file".into());
        state.mode_reveal = 1.0;
        state.new_file = Some(IdeNewFile {
            workspace: "/project".into(),
            path: "draft.rs".into(),
            error: None,
        });
        state.save(root.path()).unwrap();
        let loaded = IdeWorkspaceState::load(root.path());
        assert!(loaded.is_active());
        assert_eq!((loaded.explorer_width, loaded.agent_width), (276.0, 412.0));
        assert!(!loaded.agent_open);
        assert!(loaded.explorer_content_width.is_none());
        assert!(loaded.agent_content_width.is_none());
        let saved = std::fs::read_to_string(root.path().join("workspace-layout.json")).unwrap();
        assert!(!saved.contains("explorer_content_width"));
        assert!(!saved.contains("agent_content_width"));
        assert_eq!(loaded.activity, IdeActivity::Git);
        assert_eq!(loaded.bottom_panel, IdeBottomPanel::Problems);
        assert!(loaded.bottom_open);
        assert!(!loaded.extensions_open);
        assert!(loaded.extensions_item.is_none());
        assert!(loaded.extensions_workspace.is_none());
        assert!(!loaded.sessions_open);
        assert!(loaded.menu_open.is_none());
        assert_eq!(loaded.mode_reveal, 0.0);
        assert!(loaded.new_file.is_none());
        std::fs::write(root.path().join("workspace-layout.json"), "invalid").unwrap();
        assert_eq!(
            IdeWorkspaceState::load(root.path()).mode,
            WindowMode::Agents
        );
    }

    #[test]
    fn ide_layout_keeps_the_editor_usable_and_recovers_panels_after_resize() {
        let mut state = crate::view::tests::empty_state();
        state.ide.mode = WindowMode::Ide;
        let wide = layout(&state, 1440.0);
        assert_eq!((wide.left, wide.right), (240.0, Some(380.0)));
        for width in [320.0, 520.0, 800.0, 1024.0, 1440.0, 1920.0] {
            let value = layout(&state, width);
            let dividers = f32::from(value.left > 0.0) * SIDEBAR_GAP
                + f32::from(value.right.is_some()) * DIVIDER;
            assert!(
                (value.left
                    + value.center
                    + value.right.unwrap_or(0.0)
                    + dividers
                    + 8.0
                    + ACTIVITY_BAR_WIDTH
                    - width)
                    .abs()
                    < 0.01
            );
            assert!(value.center >= 272.0);
        }
        assert_eq!(layout(&state, 800.0).right, None);
        assert_eq!(layout(&state, 320.0).left, 0.0);
        assert_eq!(layout(&state, 1440.0), wide);
    }
}
