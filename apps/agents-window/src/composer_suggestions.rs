//! Editor completions backed by real workspace files and observed skills.
//!
//! The controller owns admission, review and attachments. This module only
//! matches a collapsed caret, retains a frozen result set and renders it with
//! openide-ui. Choosing a skill opens its existing exact-document review flow.

use crate::{files, native_skills_settings::NativeSkillsUiState, theme};
use openide_core::TurnMode;
use openide_harness::SkillSelection;
use openide_ui::{
    Component, Element, FontFamily, Icon, Kbd, Popover, PopoverPlacement, ScrollArea,
    ScrollbarAppearance, SelectorSizing, Separator, TextSpan,
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
};

const MAX_QUERY_CHARS: usize = 128;
const MAX_SUGGESTIONS: usize = 32;
const VISIBLE_SUGGESTIONS: usize = 7;
const SUGGESTION_ROW_HEIGHT: f32 = 42.0;
pub(crate) const SCROLL_ID: &str = "composer_suggestion_options";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerTriggerKind {
    Command,
    File,
}

/// Character positions, matching UiTree's Unicode-aware input caret API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposerTrigger {
    pub kind: ComposerTriggerKind,
    pub query: String,
    pub start: usize,
    pub cursor: usize,
    pub end: usize,
}

/// Slash commands start a line. File references start a token, so an email
/// address, URL or an already completed reference cannot open this panel.
pub(crate) fn trigger_at_cursor(text: &str, cursor: usize) -> Option<ComposerTrigger> {
    let chars: Vec<char> = text.chars().collect();
    if cursor > chars.len() {
        return None;
    }
    let mut start = cursor;
    while start > 0 && token_char(chars[start - 1]) {
        start -= 1;
    }
    let marker = *chars.get(start)?;
    let kind = match marker {
        '/' => {
            let line_start = chars[..start]
                .iter()
                .rposition(|ch| *ch == '\n')
                .map_or(0, |index| index + 1);
            if !chars[line_start..start].iter().all(|ch| ch.is_whitespace()) {
                return None;
            }
            ComposerTriggerKind::Command
        }
        '@' => {
            if start > 0
                && !chars[start - 1].is_whitespace()
                && !matches!(chars[start - 1], '(' | '[' | '{')
            {
                return None;
            }
            ComposerTriggerKind::File
        }
        _ => return None,
    };
    if cursor <= start || cursor - start > MAX_QUERY_CHARS + 1 {
        return None;
    }
    let query: String = chars[start + 1..cursor].iter().collect();
    if query.contains(['@', '`', '"', '\''])
        || (kind == ComposerTriggerKind::Command && query.contains('/'))
    {
        return None;
    }
    let mut end = cursor;
    while end < chars.len() && token_char(chars[end]) {
        end += 1;
    }
    Some(ComposerTrigger {
        kind,
        query,
        start,
        cursor,
        end,
    })
}

fn token_char(ch: char) -> bool {
    !ch.is_whitespace() && !matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | '`' | '"' | '\'')
}

/// A typed mode command is also meaningful after autocomplete closes at the
/// first argument space. The host applies this mode before admitting the turn
/// and sends only the remaining prompt. A mode-only command changes the draft.
pub(crate) fn leading_mode_command(text: &str) -> Option<(TurnMode, String)> {
    let text = text.trim_start();
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    let mode = if text[..end].eq_ignore_ascii_case("/plan") {
        TurnMode::Plan
    } else if text[..end].eq_ignore_ascii_case("/agent") {
        TurnMode::Build
    } else {
        return None;
    };
    Some((mode, text[end..].trim_start().to_owned()))
}

/// Revalidate the full token before accepting a frozen result. Replacement
/// includes the token suffix after the caret and leaves all other text intact.
pub(crate) fn replace_trigger(
    text: &str,
    trigger: &ComposerTrigger,
    replacement: &str,
) -> Option<(String, usize)> {
    if trigger_at_cursor(text, trigger.cursor).as_ref() != Some(trigger) {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut value: String = chars[..trigger.start].iter().collect();
    value.push_str(replacement);
    value.extend(chars[trigger.end..].iter());
    Some((value, trigger.start + replacement.chars().count()))
}

#[derive(Clone, Debug)]
pub(crate) enum ComposerSuggestionAction {
    Mode(TurnMode),
    Skills,
    Files,
    Permissions,
    /// Review the uncommitted changes in a separate read-only conversation.
    ReviewChanges,
    /// Put a plugin's command prompt in the composer for the user to send.
    PluginCommand(PathBuf),
    ReviewSkill(SkillSelection),
    ReferenceFile(PathBuf),
}

#[derive(Clone, Debug)]
pub(crate) struct ComposerSuggestion {
    pub label: String,
    pub description: String,
    pub action: ComposerSuggestionAction,
    group: &'static str,
    icon: Icon,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ComposerSuggestionsState {
    pub trigger: Option<ComposerTrigger>,
    pub workspace: PathBuf,
    pub generation: u64,
    pub items: Vec<ComposerSuggestion>,
    pub selected: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub truncated: bool,
    /// Commands of the plugins the user switched on, set by the host before
    /// each command lookup.
    pub plugin_commands: Vec<crate::compat_hook_settings::PluginCommand>,
    dismissed: Option<ComposerTrigger>,
}

impl ComposerSuggestionsState {
    /// Returns true when a new catalog/query must be submitted. Escape keeps
    /// the current token dismissed until the text or caret actually changes.
    pub fn sync(&mut self, workspace: &Path, mut trigger: Option<ComposerTrigger>) -> bool {
        if self.workspace != workspace {
            self.dismissed = None;
        }
        if trigger == self.dismissed {
            trigger = None;
        } else {
            self.dismissed = None;
        }
        if self.trigger == trigger && self.workspace == workspace {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.workspace = workspace.to_owned();
        self.trigger = trigger;
        self.items.clear();
        self.selected = 0;
        self.loading = self
            .trigger
            .as_ref()
            .is_some_and(|trigger| trigger.kind == ComposerTriggerKind::File);
        self.error = None;
        self.truncated = false;
        true
    }

    pub fn dismiss(&mut self) {
        self.dismissed = self.trigger.clone();
        self.trigger = None;
        self.items.clear();
        self.loading = false;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn populate_commands(&mut self, skills: &NativeSkillsUiState) {
        let Some(trigger) = self.trigger.as_ref() else {
            return;
        };
        if trigger.kind != ComposerTriggerKind::Command {
            return;
        }
        let query = trigger.query.to_lowercase();
        let mut candidates: Vec<(u32, usize, ComposerSuggestion)> = Vec::new();
        for (name, description, action, icon) in [
            (
                "plan",
                "Planificar sin ejecutar cambios",
                ComposerSuggestionAction::Mode(TurnMode::Plan),
                Icon::Plan,
            ),
            (
                "agent",
                "Trabajar con las herramientas y permisos elegidos",
                ComposerSuggestionAction::Mode(TurnMode::Build),
                Icon::Code,
            ),
            (
                "skills",
                "Revisar y elegir skills del proyecto",
                ComposerSuggestionAction::Skills,
                Icon::Grid,
            ),
            (
                "files",
                "Referenciar un archivo del proyecto con @",
                ComposerSuggestionAction::Files,
                Icon::File,
            ),
            (
                "permissions",
                "Configurar la aprobación de acciones",
                ComposerSuggestionAction::Permissions,
                Icon::Settings,
            ),
            (
                "review",
                "Revisar los cambios sin confirmar en una conversación aparte, solo lectura",
                ComposerSuggestionAction::ReviewChanges,
                Icon::Search,
            ),
        ] {
            if let Some(rank) = match_rank(name, description, &query) {
                let index = candidates.len();
                candidates.push((
                    rank,
                    index,
                    ComposerSuggestion {
                        label: format!("/{name}"),
                        description: description.into(),
                        action,
                        group: "Comandos",
                        icon,
                    },
                ));
            }
        }
        // The observed catalog is tied to this workspace and exact document
        // hash. A failed catalog never produces fabricated skill suggestions.
        if skills.load_error.is_none()
            && skills.catalog_error.is_none()
            && Path::new(&skills.workspace) == self.workspace
        {
            for skill in &skills.available {
                if let Some(rank) = match_rank(&skill.name, &skill.description, &query) {
                    let index = candidates.len();
                    candidates.push((
                        rank,
                        index,
                        ComposerSuggestion {
                            label: format!("/{}", skill.name),
                            description: skill.description.clone(),
                            action: ComposerSuggestionAction::ReviewSkill(skill.selection.clone()),
                            group: "Skills",
                            icon: Icon::Grid,
                        },
                    ));
                }
            }
        }
        for command in &self.plugin_commands {
            if let Some(rank) = match_rank(&command.label, &command.description, &query) {
                let index = candidates.len();
                candidates.push((
                    rank,
                    index,
                    ComposerSuggestion {
                        label: format!("/{}", command.label),
                        description: command.description.clone(),
                        action: ComposerSuggestionAction::PluginCommand(command.file.clone()),
                        group: "Plugins",
                        icon: Icon::Puzzle,
                    },
                ));
            }
        }
        candidates.sort_by_key(|(rank, index, _)| (*rank, *index));
        // Keep each catalog section contiguous. The most relevant hit decides
        // the section order, while the original ranking orders its rows.
        let mut groups = Vec::new();
        for (_, _, entry) in &candidates {
            if !groups.contains(&entry.group) {
                groups.push(entry.group);
            }
        }
        candidates.sort_by_key(|(_, _, entry)| {
            groups
                .iter()
                .position(|group| *group == entry.group)
                .unwrap_or(usize::MAX)
        });
        self.truncated = candidates.len() > MAX_SUGGESTIONS;
        self.items = candidates
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(_, _, entry)| entry)
            .collect();
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        self.loading = false;
    }

    pub fn file_request(&self) -> Option<FileSuggestionRequest> {
        let trigger = self.trigger.as_ref()?;
        (trigger.kind == ComposerTriggerKind::File).then(|| FileSuggestionRequest {
            workspace: self.workspace.clone(),
            query: trigger.query.clone(),
            generation: self.generation,
        })
    }

    pub fn accept_files(&mut self, response: FileSuggestionResponse) -> bool {
        let Some(request) = self.file_request() else {
            return false;
        };
        if response.workspace != request.workspace
            || response.generation != request.generation
            || response.query != request.query
        {
            return false;
        }
        self.loading = false;
        self.error = response.error;
        self.truncated = response.truncated || response.files.len() > MAX_SUGGESTIONS;
        self.items = response
            .files
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|relative| ComposerSuggestion {
                label: relative
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| relative.display().to_string()),
                description: relative.display().to_string(),
                icon: openide_ui::file_icon(&relative.to_string_lossy()),
                action: ComposerSuggestionAction::ReferenceFile(relative),
                group: "Archivos",
            })
            .collect();
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        true
    }

    pub fn move_selection(&mut self, delta: isize) {
        if !self.items.is_empty() {
            self.selected =
                (self.selected as isize + delta).rem_euclid(self.items.len() as isize) as usize;
        }
    }

    pub fn selected_item(&self) -> Option<&ComposerSuggestion> {
        self.items.get(self.selected)
    }

    /// Virtual row index, including section headers. The host reveals this
    /// row after a keyboard move, new query or newly completed file search.
    pub fn selected_scroll_index(&self) -> usize {
        suggestion_slots(self)
            .iter()
            .position(|slot| matches!(slot, SuggestionSlot::Item(index) if *index == self.selected))
            .unwrap_or(0)
    }

    pub fn item(&self, generation: u64, index: usize) -> Option<&ComposerSuggestion> {
        (generation == self.generation && self.trigger.is_some())
            .then(|| self.items.get(index))
            .flatten()
    }
}

#[derive(Clone, Copy)]
enum SuggestionSlot {
    Section(&'static str),
    Item(usize),
}

fn suggestion_slots(state: &ComposerSuggestionsState) -> Vec<SuggestionSlot> {
    let sectioned = state
        .trigger
        .as_ref()
        .is_some_and(|trigger| trigger.kind == ComposerTriggerKind::Command);
    let mut slots = Vec::with_capacity(state.items.len() + 2);
    let mut previous_group = None;
    for (index, item) in state.items.iter().enumerate() {
        if sectioned && previous_group != Some(item.group) {
            slots.push(SuggestionSlot::Section(item.group));
            previous_group = Some(item.group);
        }
        slots.push(SuggestionSlot::Item(index));
    }
    slots
}

fn match_rank(name: &str, description: &str, query: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    if name == query {
        return Some(0);
    }
    if name.starts_with(query) {
        return Some(1);
    }
    if name.contains(query) {
        return Some(2);
    }
    let mut remaining = query.chars();
    let mut wanted = remaining.next();
    for ch in name.chars() {
        if wanted == Some(ch) {
            wanted = remaining.next();
            if wanted.is_none() {
                return Some(3);
            }
        }
    }
    description.to_lowercase().contains(query).then_some(4)
}

#[derive(Clone, Debug)]
pub(crate) struct FileSuggestionRequest {
    pub workspace: PathBuf,
    pub query: String,
    pub generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct FileSuggestionResponse {
    pub workspace: PathBuf,
    pub query: String,
    pub generation: u64,
    pub files: Vec<PathBuf>,
    pub truncated: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct FileMailbox {
    pending: Option<FileSuggestionRequest>,
    completed: Option<FileSuggestionResponse>,
    closed: bool,
}

/// One bounded scan in progress, one newest pending query and one response.
/// No filesystem access belongs to the editor's paint or keyboard loop.
pub(crate) struct FileSuggestionWorker {
    mailbox: Arc<(Mutex<FileMailbox>, Condvar)>,
}

impl FileSuggestionWorker {
    pub fn spawn() -> io::Result<Self> {
        let mailbox = Arc::new((Mutex::new(FileMailbox::default()), Condvar::new()));
        let actor = Arc::clone(&mailbox);
        thread::Builder::new()
            .name("openide-composer-files".into())
            .spawn(move || loop {
                let request = {
                    let (mutex, ready) = &*actor;
                    let Ok(mut state) = mutex.lock() else {
                        return;
                    };
                    while !state.closed && state.pending.is_none() {
                        let Ok(next) = ready.wait(state) else {
                            return;
                        };
                        state = next;
                    }
                    if state.closed {
                        return;
                    }
                    state.pending.take().expect("pending query checked")
                };
                let response = search_files(request);
                let Ok(mut state) = actor.0.lock() else {
                    return;
                };
                if state.closed {
                    return;
                }
                state.completed = Some(response);
            })?;
        Ok(Self { mailbox })
    }

    pub fn submit(&self, request: FileSuggestionRequest) -> Result<(), String> {
        let mut state = self
            .mailbox
            .0
            .lock()
            .map_err(|_| "La búsqueda de archivos no está disponible".to_owned())?;
        if state.closed {
            return Err("La búsqueda de archivos se cerró".into());
        }
        state.pending = Some(request);
        state.completed = None;
        self.mailbox.1.notify_one();
        Ok(())
    }

    pub fn poll_latest(&self) -> Option<FileSuggestionResponse> {
        self.mailbox.0.lock().ok()?.completed.take()
    }
}

impl Drop for FileSuggestionWorker {
    fn drop(&mut self) {
        if let Ok(mut state) = self.mailbox.0.lock() {
            state.closed = true;
            state.pending = None;
        }
        self.mailbox.1.notify_one();
    }
}

fn search_files(request: FileSuggestionRequest) -> FileSuggestionResponse {
    let mut response = FileSuggestionResponse {
        workspace: request.workspace.clone(),
        query: request.query.clone(),
        generation: request.generation,
        files: Vec::new(),
        truncated: false,
        error: None,
    };
    if request.query.trim().is_empty() {
        // Bare @ is a preview of real root files, not an unbounded recursive
        // inventory. Typing a query searches nested files through the existing
        // bounded traversal, which also excludes generated directories.
        match files::list_directory(&request.workspace, Path::new("")) {
            Ok(listing) => {
                response.truncated = listing.truncated;
                response.files = listing
                    .entries
                    .into_iter()
                    .filter(|entry| !entry.is_dir)
                    .map(|entry| entry.relative)
                    .collect();
            }
            Err(error) => response.error = Some(error.to_string()),
        }
    } else {
        match files::search_project_files(&request.workspace, &request.query) {
            Ok(found) => {
                response.files = found.hits;
                response.truncated = found.truncated;
            }
            Err(error) => response.error = Some(error.to_string()),
        }
    }
    response
}

/// Anchor this to the editor/composer after its other modal controls have been
/// composed. No keyboard-focus scope or blocking backdrop is introduced.
pub(crate) fn popup(
    anchor: Element,
    state: &ComposerSuggestionsState,
    available_width: f32,
) -> Element {
    let Some(trigger) = state.trigger.as_ref() else {
        return anchor;
    };
    let sizing = SelectorSizing::CATALOG;
    let width = available_width.min(352.0).max(160.0);
    let inner_width = width - sizing.padding * 2.0;
    let mut contents = vec![Element::text(match trigger.kind {
        ComposerTriggerKind::Command => "Comandos y skills",
        ComposerTriggerKind::File => "Archivos del proyecto",
    })
    .font_family(FontFamily::Sans)
    .font_size(sizing.label_size)
    .foreground(theme::MENU_MUTED)
    .padding(sizing.padding)
    .size(Some(inner_width), Some(sizing.header_height))];
    let appearance = ScrollbarAppearance::reserved()
        .rail_width(sizing.scrollbar_gutter)
        .tint(theme::MENU_MUTED);
    // Reserve the rail before rows are built so scrollbar targets cannot
    // overlap their full-width click surface, even as overflow changes.
    let row_width = (inner_width - sizing.scrollbar_gutter - 2.0).max(0.0);
    if !state.items.is_empty() {
        let slots = Arc::new(suggestion_slots(state));
        let items = Arc::new(state.items.clone());
        let selected = state.selected;
        let generation = state.generation;
        let row_count = slots.len();
        contents.push(
            ScrollArea::uniform_list(
                SCROLL_ID,
                row_count,
                SUGGESTION_ROW_HEIGHT,
                move |slot_index| match slots[slot_index] {
                    SuggestionSlot::Section(group) => Element::column(vec![
                        Separator::horizontal(theme::component_palette()).render(),
                        Element::text(group)
                            .font_family(FontFamily::Sans)
                            .font_size(sizing.detail_size)
                            .foreground(theme::MENU_MUTED)
                            .padding(sizing.padding)
                            .flex(1.0),
                    ])
                    .size(Some(row_width), Some(SUGGESTION_ROW_HEIGHT)),
                    SuggestionSlot::Item(index) => {
                        let item = &items[index];
                        theme::menu_palette()
                            .interactive_surface(Element::clickable(
                                format!("composer_suggestion:{generation}:{index}"),
                                Element::row(vec![
                                    Element::icon(item.icon)
                                        .foreground(theme::FOREGROUND)
                                        .size(Some(sizing.icon_size), Some(30.0)),
                                    Element::column(vec![
                                        Element::truncated_text(&item.label)
                                            .font_family(FontFamily::Sans)
                                            .font_size(sizing.label_size)
                                            .foreground(theme::FOREGROUND)
                                            .size(None, Some(16.0)),
                                        Element::truncated_text(&item.description)
                                            .font_family(FontFamily::Sans)
                                            .font_size(sizing.detail_size)
                                            .foreground(theme::MENU_MUTED)
                                            .size(None, Some(14.0)),
                                    ])
                                    .flex(1.0),
                                ])
                                .gap(sizing.gap)
                                .padding(sizing.padding),
                            ))
                            .background(if index == selected {
                                theme::MENU_HOVER
                            } else {
                                theme::MENU_SURFACE
                            })
                            .transition_ms(0)
                            .size(Some(row_width), Some(SUGGESTION_ROW_HEIGHT))
                    }
                },
            )
            .appearance(appearance)
            .render()
            .min_size(0.0, SUGGESTION_ROW_HEIGHT)
            .size(
                Some(inner_width),
                Some(row_count.min(VISIBLE_SUGGESTIONS) as f32 * SUGGESTION_ROW_HEIGHT),
            )
            .flex(1.0),
        );
    } else {
        let message = if state.loading {
            "Buscando archivos…"
        } else if let Some(error) = state.error.as_deref() {
            error
        } else {
            match trigger.kind {
                ComposerTriggerKind::Command => "No hay comandos ni skills que coincidan.",
                ComposerTriggerKind::File if trigger.query.is_empty() => {
                    "Escribí un nombre para buscar dentro del proyecto."
                }
                ComposerTriggerKind::File => "No hay archivos que coincidan.",
            }
        };
        contents.push(
            ScrollArea::new(
                SCROLL_ID,
                Element::formatted_text(vec![TextSpan::new(
                    message,
                    theme::MENU_MUTED,
                    sizing.label_size,
                    FontFamily::Sans,
                )])
                .padding(8.0)
                .size(Some(row_width), None),
            )
            .appearance(appearance)
            .render()
            .min_size(0.0, SUGGESTION_ROW_HEIGHT)
            .size(Some(inner_width), Some(SUGGESTION_ROW_HEIGHT))
            .flex(1.0),
        );
    }
    contents.push(Separator::horizontal(theme::component_palette()).render());
    let hint = |keys: &[&str], label: &str| {
        let mut parts = Vec::new();
        for (index, key) in keys.iter().enumerate() {
            if index > 0 {
                parts.push(
                    Element::text("/")
                        .font_size(11.0)
                        .foreground(theme::MENU_MUTED),
                );
            }
            parts.push(Kbd::new(*key, theme::menu_palette()).render());
        }
        parts.push(
            Element::text(label)
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(theme::MENU_MUTED),
        );
        Element::row(parts).gap(4.0).size(None, Some(20.0))
    };
    let hints = vec![
        hint(&["↑↓"], "Elegir"),
        hint(&["Enter", "Tab"], "Aceptar"),
        hint(&["Esc"], "Cerrar"),
    ];
    contents.push(
        if width < 300.0 {
            Element::column(hints).gap(4.0)
        } else {
            Element::row(hints).gap(10.0)
        }
        .padding(4.0)
        .size(Some(inner_width), None),
    );
    if state.truncated {
        contents.push(
            Element::text("Resultados limitados")
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(theme::MENU_MUTED)
                .padding(4.0),
        );
    }
    Popover::new(
        anchor,
        Element::column(contents).gap(2.0),
        true,
        "composer_suggestion_dismiss",
        theme::menu_palette(),
    )
    .width(width)
    .placement(PopoverPlacement::TopStart)
    .animation_ms(80)
    .close_animation_ms(60)
    .render()
    .popover_non_modal()
}
