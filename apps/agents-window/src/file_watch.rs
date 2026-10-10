//! Tells the file tree when a directory it shows gained, lost or renamed an entry.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecursiveMode, Watcher};

/// Watches exactly the directories the tree has loaded, without recursion.
/// A large ignored folder costs nothing until the user expands it.
pub struct TreeWatcher {
    watcher: notify::RecommendedWatcher,
    watched: HashSet<PathBuf>,
    changed: Arc<AtomicBool>,
    wake: Arc<Mutex<Option<Box<dyn Fn() + Send>>>>,
}

impl TreeWatcher {
    /// `None` when the platform offers no file notifications.
    pub fn new() -> Option<Self> {
        let changed = Arc::new(AtomicBool::new(false));
        let wake: Arc<Mutex<Option<Box<dyn Fn() + Send>>>> = Arc::default();
        let (flag, waker) = (changed.clone(), wake.clone());
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            // Edits to a file's contents do not change the tree.
            let structural = event.is_ok_and(|event| {
                matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(notify::event::ModifyKind::Name(_))
                )
            });
            if structural && !flag.swap(true, Ordering::AcqRel) {
                if let Ok(wake) = waker.lock() {
                    if let Some(wake) = wake.as_ref() {
                        wake();
                    }
                }
            }
        })
        .ok()?;
        Some(Self {
            watcher,
            watched: HashSet::new(),
            changed,
            wake,
        })
    }

    /// Called from the watcher's thread when the tree becomes stale.
    pub fn on_change(&self, wake: impl Fn() + Send + 'static) {
        if let Ok(mut slot) = self.wake.lock() {
            if slot.is_none() {
                *slot = Some(Box::new(wake));
            }
        }
    }

    /// Makes the watched set equal to `directories`. A directory that cannot
    /// be watched, for instance one deleted a moment ago, is skipped.
    pub fn watch(&mut self, directories: impl IntoIterator<Item = PathBuf>) {
        let wanted: HashSet<PathBuf> = directories.into_iter().collect();
        if wanted == self.watched {
            return;
        }
        for gone in self.watched.difference(&wanted) {
            let _ = self.watcher.unwatch(gone);
        }
        let mut watched = HashSet::new();
        for directory in wanted {
            if self.watched.contains(&directory)
                || self
                    .watcher
                    .watch(&directory, RecursiveMode::NonRecursive)
                    .is_ok()
            {
                watched.insert(directory);
            }
        }
        self.watched = watched;
    }

    /// True once after any structural change since the last call.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(watcher: &TreeWatcher) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if watcher.take_changed() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn entries_created_renamed_and_removed_in_watched_folders_are_reported() {
        let root = tempfile::tempdir().unwrap();
        let (shown, hidden) = (root.path().join("shown"), root.path().join("hidden"));
        std::fs::create_dir_all(&shown).unwrap();
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(shown.join("kept.txt"), "one").unwrap();
        let Some(mut watcher) = TreeWatcher::new() else {
            return;
        };
        let woken = Arc::new(AtomicBool::new(false));
        let flag = woken.clone();
        watcher.on_change(move || flag.store(true, Ordering::Release));
        watcher.watch([root.path().to_path_buf(), shown.clone()]);
        assert!(!watcher.take_changed());

        std::fs::write(shown.join("new.txt"), "x").unwrap();
        assert!(wait_for(&watcher), "created");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !woken.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(woken.load(Ordering::Acquire));
        std::fs::rename(shown.join("new.txt"), shown.join("renamed.txt")).unwrap();
        assert!(wait_for(&watcher), "renamed");
        std::fs::remove_file(shown.join("renamed.txt")).unwrap();
        assert!(wait_for(&watcher), "removed");
        std::fs::create_dir(root.path().join("top")).unwrap();
        assert!(wait_for(&watcher), "created in the root");

        // Rewriting a file, or changing a folder that is not shown, is silent.
        std::thread::sleep(Duration::from_millis(100));
        watcher.take_changed();
        std::fs::write(shown.join("kept.txt"), "two").unwrap();
        std::fs::write(hidden.join("inside.txt"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!watcher.take_changed());

        // A folder that stops being shown stops reporting; a new one starts.
        watcher.watch([root.path().to_path_buf(), hidden.clone()]);
        std::fs::write(shown.join("ignored.txt"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!watcher.take_changed());
        std::fs::remove_file(hidden.join("inside.txt")).unwrap();
        assert!(wait_for(&watcher), "removed in the newly watched folder");
        watcher.watch([root.path().join("missing")]);
    }
}
