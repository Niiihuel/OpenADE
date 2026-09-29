//! Search model for the Agents Window command center.
//!
//! The UI owns focus, overlay placement, and keyboard interaction. This module
//! only turns commands, sessions, and visible workspace files into ranked,
//! typed navigation results. It does not read the filesystem or execute an
//! action when a result is selected.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const DEFAULT_RESULT_LIMIT: usize = 12;
pub const MAX_RESULT_LIMIT: usize = 80;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Scope {
    #[default]
    All,
    Commands,
    Sessions,
    Files,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedQuery {
    pub scope: Scope,
    pub text: String,
    pub explicit_scope: bool,
}

/// A leading `>`, `#`, or `@` selects commands, sessions, or files. Without
/// one, the manually selected scope remains in effect.
pub fn parse_query(raw: &str, selected_scope: Scope) -> ParsedQuery {
    let trimmed = raw.trim_start();
    let prefixed = match trimmed.chars().next() {
        Some('>') => Some(Scope::Commands),
        Some('#') => Some(Scope::Sessions),
        Some('@') => Some(Scope::Files),
        _ => None,
    };
    if let Some(scope) = prefixed {
        ParsedQuery {
            scope,
            text: trimmed[1..].trim().to_owned(),
            explicit_scope: true,
        }
    } else {
        ParsedQuery {
            scope: selected_scope,
            text: raw.trim().to_owned(),
            explicit_scope: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    NewTask,
    OpenSessions,
    OpenFiles,
    OpenSettings,
    OpenAutomations,
    OpenMarketplace,
    OpenCodeReview,
    CompactContext,
    ResumeTurn,
    ToggleRightDock,
    OpenSession(String),
    OpenFile(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultItem {
    pub target: Target,
    pub section: Scope,
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Copy, Debug)]
pub struct SessionCandidate<'a> {
    pub id: &'a str,
    pub title: &'a str,
    /// Project, backend, or a search excerpt from the session store.
    pub subtitle: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub struct FileCandidate<'a> {
    /// Workspace-relative path. The caller must validate it when opening.
    pub path: &'a Path,
    pub name: &'a str,
    pub is_dir: bool,
}

#[derive(Clone, Copy)]
struct StaticCommand {
    target: StaticTarget,
    title: &'static str,
    subtitle: &'static str,
    keywords: &'static str,
}

#[derive(Clone, Copy)]
enum StaticTarget {
    NewTask,
    OpenSessions,
    OpenFiles,
    OpenSettings,
    OpenAutomations,
    OpenMarketplace,
    OpenCodeReview,
    CompactContext,
    ResumeTurn,
    ToggleRightDock,
}

impl From<StaticTarget> for Target {
    fn from(value: StaticTarget) -> Self {
        match value {
            StaticTarget::NewTask => Self::NewTask,
            StaticTarget::OpenSessions => Self::OpenSessions,
            StaticTarget::OpenFiles => Self::OpenFiles,
            StaticTarget::OpenSettings => Self::OpenSettings,
            StaticTarget::OpenAutomations => Self::OpenAutomations,
            StaticTarget::OpenMarketplace => Self::OpenMarketplace,
            StaticTarget::OpenCodeReview => Self::OpenCodeReview,
            StaticTarget::CompactContext => Self::CompactContext,
            StaticTarget::ResumeTurn => Self::ResumeTurn,
            StaticTarget::ToggleRightDock => Self::ToggleRightDock,
        }
    }
}

const COMMANDS: [StaticCommand; 10] = [
    StaticCommand {
        target: StaticTarget::NewTask,
        title: "Nueva tarea",
        subtitle: "Crear una sesión",
        keywords: "new task create session nueva sesion",
    },
    StaticCommand {
        target: StaticTarget::OpenSessions,
        title: "Sesiones",
        subtitle: "Abrir la lista de tareas",
        keywords: "tasks conversations history historial sesiones",
    },
    StaticCommand {
        target: StaticTarget::OpenFiles,
        title: "Archivos",
        subtitle: "Explorar archivos del proyecto",
        keywords: "files workspace browser explorer proyecto",
    },
    StaticCommand {
        target: StaticTarget::OpenSettings,
        title: "Configuración",
        subtitle: "Abrir preferencias del agente",
        keywords: "settings preferences configure config",
    },
    StaticCommand {
        target: StaticTarget::OpenAutomations,
        title: "Automatizaciones",
        subtitle: "Programar y administrar tareas recurrentes",
        keywords: "automations schedules recurring tareas programadas",
    },
    StaticCommand {
        target: StaticTarget::OpenMarketplace,
        title: "Marketplace",
        subtitle: "Instalar y administrar skills desde fuentes abiertas",
        keywords: "marketplace skills plugins extensiones catalogo",
    },
    StaticCommand {
        target: StaticTarget::OpenCodeReview,
        title: "Revisar cambios",
        subtitle: "Abrir el diff de Git del proyecto",
        keywords: "review code diff git changes cambios revisar",
    },
    StaticCommand {
        target: StaticTarget::CompactContext,
        title: "Compactar contexto",
        subtitle: "Resumir el historial de la conversación activa",
        keywords: "compact compaction summarize resumen contexto context historial tokens",
    },
    StaticCommand {
        target: StaticTarget::ResumeTurn,
        title: "Reanudar turno interrumpido",
        subtitle: "Recuperar un turno nativo sin repetir efectos inciertos",
        keywords: "resume reanudar recuperar interrupted turno interrumpido recovery",
    },
    StaticCommand {
        target: StaticTarget::ToggleRightDock,
        title: "Alternar panel derecho",
        subtitle: "Mostrar u ocultar contexto y actividad",
        keywords: "right dock sidebar context activity panel",
    },
];

struct ScoredItem {
    item: ResultItem,
    score: u32,
    initial_order: usize,
}

/// Search the built-in commands, supplied sessions, and visible file rows.
/// Empty unscoped queries show commands followed by sessions in caller order
/// (normally most recently updated first). Files require a query or `@`.
pub fn search(
    raw_query: &str,
    selected_scope: Scope,
    sessions: &[SessionCandidate<'_>],
    files: &[FileCandidate<'_>],
    limit: usize,
) -> Vec<ResultItem> {
    let parsed = parse_query(raw_query, selected_scope);
    let query = fold(&parsed.text);
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let show_files = !tokens.is_empty() || parsed.scope == Scope::Files;
    let mut found = Vec::new();

    if matches!(parsed.scope, Scope::All | Scope::Commands) {
        for (index, command) in COMMANDS.iter().enumerate() {
            if let Some(score) = match_score(
                &query,
                &tokens,
                command.title,
                command.subtitle,
                command.keywords,
            ) {
                found.push(ScoredItem {
                    item: ResultItem {
                        target: command.target.into(),
                        section: Scope::Commands,
                        title: command.title.to_owned(),
                        subtitle: command.subtitle.to_owned(),
                    },
                    score,
                    initial_order: index,
                });
            }
        }
    }

    if matches!(parsed.scope, Scope::All | Scope::Sessions) {
        let mut seen = HashSet::new();
        for (index, session) in sessions.iter().enumerate() {
            if !seen.insert(session.id) {
                continue;
            }
            if let Some(score) = match_score(&query, &tokens, session.title, session.subtitle, "") {
                found.push(ScoredItem {
                    item: ResultItem {
                        target: Target::OpenSession(session.id.to_owned()),
                        section: Scope::Sessions,
                        title: session.title.to_owned(),
                        subtitle: session.subtitle.to_owned(),
                    },
                    score,
                    initial_order: index,
                });
            }
        }
    }

    if show_files && matches!(parsed.scope, Scope::All | Scope::Files) {
        let mut seen = HashSet::new();
        for (index, file) in files.iter().enumerate() {
            if file.is_dir || !seen.insert(file.path) {
                continue;
            }
            let path = file.path.to_string_lossy();
            if let Some(score) = match_score(&query, &tokens, file.name, &path, "") {
                found.push(ScoredItem {
                    item: ResultItem {
                        target: Target::OpenFile(file.path.to_path_buf()),
                        section: Scope::Files,
                        title: file.name.to_owned(),
                        subtitle: path.into_owned(),
                    },
                    score,
                    initial_order: index,
                });
            }
        }
    }

    if !query.is_empty() {
        found.sort_by(|left, right| {
            left.score
                .cmp(&right.score)
                .then_with(|| {
                    section_order(left.item.section).cmp(&section_order(right.item.section))
                })
                .then_with(|| fold(&left.item.title).cmp(&fold(&right.item.title)))
                .then_with(|| target_key(&left.item.target).cmp(&target_key(&right.item.target)))
        });
    } else {
        found.sort_by_key(|result| (section_order(result.item.section), result.initial_order));
    }

    found
        .into_iter()
        .take(limit.min(MAX_RESULT_LIMIT))
        .map(|scored| scored.item)
        .collect()
}

fn section_order(scope: Scope) -> u8 {
    match scope {
        Scope::Commands => 0,
        Scope::Sessions => 1,
        Scope::Files => 2,
        Scope::All => 3,
    }
}

fn target_key(target: &Target) -> String {
    match target {
        Target::NewTask => "0".to_owned(),
        Target::OpenSessions => "1".to_owned(),
        Target::OpenFiles => "2".to_owned(),
        Target::OpenSettings => "3".to_owned(),
        Target::OpenCodeReview => "4".to_owned(),
        Target::CompactContext => "5".to_owned(),
        Target::ResumeTurn => "b".to_owned(),
        Target::ToggleRightDock => "6".to_owned(),
        Target::OpenAutomations => "7".to_owned(),
        Target::OpenMarketplace => "8".to_owned(),
        Target::OpenSession(id) => format!("9:{id}"),
        Target::OpenFile(path) => format!("a:{}", path.to_string_lossy()),
    }
}

/// Match every word of the query. Exact and leading title matches outrank
/// matches found only in a subtitle or keyword list.
fn match_score(
    query: &str,
    tokens: &[&str],
    title: &str,
    subtitle: &str,
    keywords: &str,
) -> Option<u32> {
    if tokens.is_empty() {
        return Some(0);
    }
    let title = fold(title);
    let subtitle = fold(subtitle);
    let keywords = fold(keywords);
    if title == query {
        return Some(0);
    }
    if title.starts_with(query) {
        return Some(1);
    }
    if word_starts_with(&title, query) {
        return Some(2);
    }
    if title.contains(query) {
        return Some(3);
    }

    let mut score = 10;
    for token in tokens {
        score += if title.starts_with(token) {
            1
        } else if word_starts_with(&title, token) {
            2
        } else if title.contains(token) {
            3
        } else if word_starts_with(&subtitle, token) {
            5
        } else if subtitle.contains(token) {
            6
        } else if word_starts_with(&keywords, token) {
            7
        } else if keywords.contains(token) {
            8
        } else {
            return None;
        };
    }
    Some(score)
}

fn word_starts_with(text: &str, query: &str) -> bool {
    text.split(|ch: char| !ch.is_alphanumeric())
        .any(|word| word.starts_with(query))
}

fn fold(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|ch| match ch {
            'á' | 'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            _ => ch,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session<'a>(id: &'a str, title: &'a str, subtitle: &'a str) -> SessionCandidate<'a> {
        SessionCandidate {
            id,
            title,
            subtitle,
        }
    }

    #[test]
    fn prefixes_override_manual_scope() {
        assert_eq!(
            parse_query("  > config  ", Scope::Files),
            ParsedQuery {
                scope: Scope::Commands,
                text: "config".into(),
                explicit_scope: true,
            }
        );
        assert_eq!(
            parse_query("#error", Scope::Commands).scope,
            Scope::Sessions
        );
        assert_eq!(parse_query("@Cargo", Scope::All).scope, Scope::Files);
        assert_eq!(parse_query("sesiones", Scope::Files).scope, Scope::Files);
    }

    #[test]
    fn empty_query_shows_commands_then_recent_sessions_without_files() {
        let sessions = [
            session("b", "Reciente", "Proyecto B"),
            session("a", "Anterior", "Proyecto A"),
        ];
        let files = [FileCandidate {
            path: Path::new("src/main.rs"),
            name: "main.rs",
            is_dir: false,
        }];
        let result = search("", Scope::All, &sessions, &files, DEFAULT_RESULT_LIMIT);
        assert_eq!(result.len(), COMMANDS.len() + 2);
        assert_eq!(result[0].target, Target::NewTask);
        assert_eq!(
            result[COMMANDS.len()].target,
            Target::OpenSession("b".into())
        );
        assert_eq!(
            result[COMMANDS.len() + 1].target,
            Target::OpenSession("a".into())
        );
    }

    #[test]
    fn exact_accent_insensitive_title_outranks_subtitle() {
        let sessions = [session("x", "Otra tarea", "Configuracion del proyecto")];
        let result = search(
            "configuracion",
            Scope::All,
            &sessions,
            &[],
            DEFAULT_RESULT_LIMIT,
        );
        assert_eq!(result[0].target, Target::OpenSettings);
        assert_eq!(result[1].target, Target::OpenSession("x".into()));
    }

    #[test]
    fn git_review_is_reachable_from_command_search() {
        let results = search("git diff", Scope::All, &[], &[], DEFAULT_RESULT_LIMIT);
        assert_eq!(
            results.first().map(|item| &item.target),
            Some(&Target::OpenCodeReview)
        );
    }

    #[test]
    fn compact_context_is_visible_and_searchable_as_a_command() {
        let defaults = search("", Scope::All, &[], &[], DEFAULT_RESULT_LIMIT);
        assert!(defaults
            .iter()
            .any(|item| item.target == Target::CompactContext && item.section == Scope::Commands));
        let exact = search(
            ">compactar contexto",
            Scope::Files,
            &[],
            &[],
            DEFAULT_RESULT_LIMIT,
        );
        assert_eq!(
            exact.first().map(|item| &item.target),
            Some(&Target::CompactContext)
        );
        let english = search(
            "summarize context",
            Scope::Commands,
            &[],
            &[],
            DEFAULT_RESULT_LIMIT,
        );
        assert_eq!(
            english.first().map(|item| &item.target),
            Some(&Target::CompactContext)
        );
    }

    #[test]
    fn all_query_words_can_match_across_title_and_subtitle() {
        let sessions = [
            session("yes", "Reparar interfaz", "Proyecto alfa"),
            session("no", "Reparar interfaz", "Proyecto beta"),
        ];
        let result = search(
            "reparar alfa",
            Scope::Sessions,
            &sessions,
            &[],
            DEFAULT_RESULT_LIMIT,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].target, Target::OpenSession("yes".into()));
    }

    #[test]
    fn matching_order_is_stable_independent_of_input_order() {
        let a = session("a", "Panel uno", "Proyecto");
        let b = session("b", "Panel dos", "Proyecto");
        let forward = search("panel", Scope::Sessions, &[a, b], &[], DEFAULT_RESULT_LIMIT);
        let backward = search("panel", Scope::Sessions, &[b, a], &[], DEFAULT_RESULT_LIMIT);
        assert_eq!(forward, backward);
    }

    #[test]
    fn files_are_filtered_and_return_relative_paths() {
        let files = [
            FileCandidate {
                path: Path::new("src"),
                name: "src",
                is_dir: true,
            },
            FileCandidate {
                path: Path::new("src/main.rs"),
                name: "main.rs",
                is_dir: false,
            },
            FileCandidate {
                path: Path::new("docs/guide.md"),
                name: "guide.md",
                is_dir: false,
            },
        ];
        let result = search("@main", Scope::All, &[], &files, DEFAULT_RESULT_LIMIT);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].target,
            Target::OpenFile(PathBuf::from("src/main.rs"))
        );
        assert_eq!(result[0].section, Scope::Files);
    }

    #[test]
    fn duplicate_sessions_are_removed_and_results_are_capped() {
        let sessions = [session("same", "Uno", ""), session("same", "Duplicado", "")];
        assert_eq!(
            search("#", Scope::All, &sessions, &[], DEFAULT_RESULT_LIMIT).len(),
            1
        );
        assert!(search("", Scope::All, &sessions, &[], 0).is_empty());

        let ids: Vec<_> = (0..100).map(|index| format!("session-{index}")).collect();
        let many: Vec<_> = ids.iter().map(|id| session(id, "Tarea", "")).collect();
        assert_eq!(
            search("#", Scope::All, &many, &[], usize::MAX).len(),
            MAX_RESULT_LIMIT
        );
    }
}
