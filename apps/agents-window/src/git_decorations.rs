//! What version control says of a file or a folder, for the file tree and
//! the file tabs. It is read from the review snapshot the source control
//! panel shows, indexed once per snapshot; nothing here runs Git.
use crate::git_review::{ChangeGroup, GitReview};
use crate::theme;
use openide_ui::Color;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Weak};

/// The letter of a file's state and the color that state has.
pub(crate) fn status_mark(group: ChangeGroup, status: &str) -> (&'static str, Color) {
    if group == ChangeGroup::Untracked {
        return ("U", theme::DIFF_ADDED);
    }
    match status {
        "Nuevo" => ("A", theme::DIFF_ADDED),
        "Eliminado" => ("D", theme::DIFF_REMOVED),
        "Renombrado" => ("R", theme::LINK),
        "Copiado" => ("C", theme::LINK),
        "Conflicto" => ("!", theme::DESTRUCTIVE),
        "Binario" => ("B", theme::FOREGROUND_SUBTLE),
        "Tipo cambiado" => ("T", theme::WARNING),
        _ => ("M", theme::WARNING),
    }
}

/// How a path is marked: the letter and color of [`status_mark`], and the
/// state in words for a hint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Decoration {
    pub letter: &'static str,
    pub color: Color,
    pub label: &'static str,
    /// Which state a folder shows among its files': the highest. Zero stays
    /// on its own path, as a deleted file does.
    rank: u8,
}

fn decoration(group: ChangeGroup, status: &str) -> Decoration {
    let (letter, color) = status_mark(group, status);
    let (label, rank) = match letter {
        "!" => ("En conflicto", 3),
        "M" => ("Modificado", 2),
        "T" => ("Tipo cambiado", 2),
        "C" => ("Copiado", 2),
        "U" => ("Sin seguimiento", 1),
        "A" => ("Añadido", 1),
        "R" => ("Renombrado", 1),
        "B" => ("Binario modificado", 1),
        _ => ("Eliminado", 0),
    };
    Decoration { letter, color, label, rank }
}

/// The decorations of one snapshot, by path relative to the workspace with
/// `/` between names.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct GitDecorations {
    files: HashMap<String, Decoration>,
    folders: HashMap<String, Decoration>,
}

impl GitDecorations {
    pub(crate) fn new(review: &GitReview) -> Self {
        let mut files = HashMap::new();
        // The work tree's state replaces the index's, and a conflict both.
        for group in [
            ChangeGroup::Staged,
            ChangeGroup::Changes,
            ChangeGroup::Untracked,
            ChangeGroup::Conflicts,
        ] {
            let range = review.group(group);
            for index in 0..range.len {
                let (path, status) = if group == ChangeGroup::Untracked {
                    (review.untracked[index].as_str(), "")
                } else {
                    let file = &review.sections[range.section].files[range.start + index];
                    (file.path.as_str(), file.status.as_str())
                };
                files.insert(path.to_owned(), decoration(group, status));
            }
        }
        let mut folders = HashMap::<String, Decoration>::new();
        for (path, mark) in files.iter().filter(|(_, mark)| mark.rank > 0) {
            let mut folder = path.as_str();
            while let Some((parent, _)) = folder.rsplit_once('/') {
                folder = parent;
                match folders.get_mut(folder) {
                    Some(held) if held.rank >= mark.rank => {}
                    Some(held) => *held = *mark,
                    None => {
                        folders.insert(folder.to_owned(), *mark);
                    }
                }
            }
        }
        Self { files, folders }
    }

    pub(crate) fn file(&self, path: &str) -> Option<Decoration> {
        self.files.get(path).copied()
    }

    /// The strongest state among the files anywhere inside `path`.
    pub(crate) fn folder(&self, path: &str) -> Option<Decoration> {
        self.folders.get(path).copied()
    }

    /// The decoration of the file at `full_path`, when it is in `workspace`.
    pub(crate) fn file_at(&self, workspace: &str, full_path: &str) -> Option<Decoration> {
        let relative = Path::new(full_path).strip_prefix(workspace).ok()?;
        self.file(&relative.to_str()?.replace('\\', "/"))
    }
}

thread_local! {
    /// The index of the snapshot on screen. Holding the snapshot weakly
    /// keeps its address from naming another one.
    static INDEXED: RefCell<Option<(Weak<GitReview>, Arc<GitDecorations>)>> =
        const { RefCell::new(None) };
}

/// The decorations of `review`, indexed the first time that snapshot is seen
/// and shared by every frame until another one arrives.
pub(crate) fn of(review: &Arc<GitReview>) -> Arc<GitDecorations> {
    INDEXED.with(|indexed| {
        let mut indexed = indexed.borrow_mut();
        if let Some((seen, decorations)) = indexed.as_ref() {
            if Weak::as_ptr(seen) == Arc::as_ptr(review) {
                return decorations.clone();
            }
        }
        let decorations = Arc::new(GitDecorations::new(review));
        *indexed = Some((Arc::downgrade(review), decorations.clone()));
        decorations
    })
}

/// None outside a repository and until its first snapshot arrives.
pub(crate) fn for_state(state: &crate::view::ViewState) -> Option<Arc<GitDecorations>> {
    match &state.git_review {
        Some(Ok(review)) => Some(of(review)),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::git_review::{ReviewFile, ReviewSection};

    fn file(path: &str, status: &str) -> ReviewFile {
        ReviewFile {
            path: path.into(),
            status: status.into(),
            ..Default::default()
        }
    }

    pub(crate) fn review(staged: &[(&str, &str)], unstaged: &[(&str, &str)], untracked: &[&str]) -> GitReview {
        let section = |title: &str, files: &[(&str, &str)]| ReviewSection {
            title: title.into(),
            files: files.iter().map(|(path, status)| file(path, status)).collect(),
            ..Default::default()
        };
        let mut review = GitReview {
            sections: vec![
                section("CAMBIOS PREPARADOS", staged),
                section("CAMBIOS SIN PREPARAR", unstaged),
            ],
            untracked: untracked.iter().map(|path| (*path).to_owned()).collect(),
            ..Default::default()
        };
        review.group_conflicts();
        review
    }

    #[test]
    fn every_status_has_the_panels_letter_and_color_and_its_name_in_words() {
        let decorations = GitDecorations::new(&review(
            &[("added.rs", "Nuevo"), ("moved.rs", "Renombrado")],
            &[
                ("changed.rs", "Modificado"),
                ("gone.rs", "Eliminado"),
                ("both.rs", "Conflicto"),
                ("mode.sh", "Tipo cambiado"),
            ],
            &["notes.md"],
        ));
        for (path, letter, color, label) in [
            ("added.rs", "A", theme::DIFF_ADDED, "Añadido"),
            ("moved.rs", "R", theme::LINK, "Renombrado"),
            ("changed.rs", "M", theme::WARNING, "Modificado"),
            ("gone.rs", "D", theme::DIFF_REMOVED, "Eliminado"),
            ("both.rs", "!", theme::DESTRUCTIVE, "En conflicto"),
            ("mode.sh", "T", theme::WARNING, "Tipo cambiado"),
            ("notes.md", "U", theme::DIFF_ADDED, "Sin seguimiento"),
        ] {
            let mark = decorations.file(path).unwrap_or_else(|| panic!("{path}"));
            assert_eq!((mark.letter, mark.color, mark.label), (letter, color, label), "{path}");
        }
        assert_eq!(decorations.file("clean.rs"), None);
        assert_eq!(decorations.folder(""), None);
    }

    #[test]
    fn the_work_tree_replaces_the_index_and_a_conflict_replaces_both() {
        let decorations = GitDecorations::new(&review(
            &[("a.rs", "Nuevo"), ("b.rs", "Modificado"), ("c.rs", "Modificado")],
            &[("a.rs", "Modificado"), ("c.rs", "Conflicto")],
            &[],
        ));
        assert_eq!(decorations.file("a.rs").unwrap().letter, "M");
        assert_eq!(decorations.file("b.rs").unwrap().letter, "M");
        assert_eq!(decorations.file("c.rs").unwrap().letter, "!");
    }

    #[test]
    fn a_folder_shows_the_strongest_state_inside_it_at_any_depth() {
        let decorations = GitDecorations::new(&review(
            &[("app/new/added.rs", "Nuevo")],
            &[
                ("app/src/deep/changed.rs", "Modificado"),
                ("lib/merge/both.rs", "Conflicto"),
                ("lib/changed.rs", "Modificado"),
                ("old/gone.rs", "Eliminado"),
            ],
            &["app/src/notes.md", "docs/draft.md"],
        ));
        let folder = |path: &str| decorations.folder(path).map(|mark| mark.letter);
        // Untracked and added are weaker than modified; a conflict wins.
        assert_eq!(folder("app"), Some("M"));
        assert_eq!(folder("app/src"), Some("M"));
        assert_eq!(folder("app/src/deep"), Some("M"));
        assert_eq!(folder("app/new"), Some("A"));
        assert_eq!(folder("docs"), Some("U"));
        assert_eq!(folder("lib"), Some("!"));
        assert_eq!(folder("lib/merge"), Some("!"));
        // A deleted file marks no folder, and a file is not a folder.
        assert_eq!(folder("old"), None);
        assert_eq!(folder("lib/changed.rs"), None);
        assert_eq!(decorations.folder("app").unwrap().color, theme::WARNING);
    }

    #[test]
    fn a_snapshot_is_indexed_once_and_a_new_one_again() {
        let first = Arc::new(review(&[], &[("a.rs", "Modificado")], &[]));
        let indexed = of(&first);
        assert!(Arc::ptr_eq(&indexed, &of(&first)));
        assert!(Arc::ptr_eq(&indexed, &of(&first.clone())));
        let second = Arc::new(review(&[], &[("b.rs", "Modificado")], &[]));
        let reindexed = of(&second);
        assert!(!Arc::ptr_eq(&indexed, &reindexed));
        assert_eq!(reindexed.file("a.rs"), None);
        assert!(Arc::ptr_eq(&reindexed, &of(&second)));
        // The first index was dropped with its turn; its snapshot's address
        // may not stand for the next one to be allocated there.
        drop((first, indexed));
        let third = Arc::new(review(&[], &[("c.rs", "Modificado")], &[]));
        assert!(of(&third).file("c.rs").is_some());
    }

    #[test]
    fn no_repository_and_no_snapshot_have_no_decorations() {
        let mut state = crate::view::tests::empty_state();
        assert!(for_state(&state).is_none());
        state.git_review = Some(Err(crate::git_review::NOT_A_REPOSITORY.into()));
        assert!(for_state(&state).is_none());
        state.git_review = Some(Ok(Arc::new(review(&[], &[("src/a.rs", "Modificado")], &[]))));
        state.workspace = "/work/project".into();
        let decorations = for_state(&state).unwrap();
        assert_eq!(
            decorations.file_at(&state.workspace, "/work/project/src/a.rs").map(|mark| mark.letter),
            Some("M")
        );
        assert_eq!(decorations.file_at(&state.workspace, "/elsewhere/src/a.rs"), None);
    }
}
