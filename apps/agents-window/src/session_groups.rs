//! User-created task groups for the sidebar.
//!
//! Group membership is presentation metadata. Sessions remain in the session
//! store, and removing a group only places its members back at the root.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

const FILE_NAME: &str = "session-groups.json";
const VERSION: u64 = 1;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_GROUPS: usize = 256;
const MAX_MEMBERS: usize = 16_384;
const MAX_TITLE_CHARS: usize = 80;
const MAX_SESSION_ID_BYTES: usize = 256;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGroup {
    pub id: String,
    pub title: String,
    pub collapsed: bool,
}

/// Groups are ordered by their position in `groups`; each session has at most
/// one membership. Sessions absent from `members` appear at the top level.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionGroups {
    pub groups: Vec<SessionGroup>,
    pub members: BTreeMap<String, String>,
    next_id: u64,
}

impl SessionGroups {
    pub fn load(root: &Path) -> Self {
        let path = root.join(FILE_NAME);
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_FILE_BYTES) {
            return Self::default();
        }
        let Some(value) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            return Self::default();
        };
        Self::from_value(&value)
    }

    fn from_value(value: &Value) -> Self {
        if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
            return Self::default();
        }
        let mut result = Self {
            next_id: value.get("next_id").and_then(Value::as_u64).unwrap_or(1),
            ..Self::default()
        };
        if let Some(groups) = value.get("groups").and_then(Value::as_array) {
            for group in groups.iter().take(MAX_GROUPS) {
                let (Some(id), Some(title)) = (
                    group.get("id").and_then(Value::as_str),
                    group.get("title").and_then(Value::as_str),
                ) else {
                    continue;
                };
                result.groups.push(SessionGroup {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    collapsed: group
                        .get("collapsed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
        }
        if let Some(members) = value.get("members").and_then(Value::as_object) {
            for (session_id, group_id) in members.iter().take(MAX_MEMBERS) {
                if let Some(group_id) = group_id.as_str() {
                    result
                        .members
                        .insert(session_id.to_owned(), group_id.to_owned());
                }
            }
        }
        result.validate();
        result
    }

    pub fn save(&self, root: &Path) -> io::Result<()> {
        let mut snapshot = self.clone();
        snapshot.validate();
        std::fs::create_dir_all(root)?;
        let groups: Vec<_> = snapshot
            .groups
            .iter()
            .map(|group| {
                json!({
                    "id": group.id,
                    "title": group.title,
                    "collapsed": group.collapsed,
                })
            })
            .collect();
        let payload = serde_json::to_vec_pretty(&json!({
            "version": VERSION,
            "next_id": snapshot.next_id,
            "groups": groups,
            "members": snapshot.members,
        }))?;
        if payload.len() as u64 > MAX_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session groups exceed persistence limit",
            ));
        }
        let temp = root.join(format!(
            ".session-groups-{}-{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&payload)?;
            file.sync_all()?;
            std::fs::rename(&temp, root.join(FILE_NAME))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    /// Create a group at the start of the manual list and return its stable id.
    pub fn create(&mut self, title: &str) -> Option<String> {
        let title = clean_title(title)?;
        if self.groups.len() >= MAX_GROUPS {
            return None;
        }
        let id = loop {
            let sequence = self.next_id.max(1);
            self.next_id = sequence.checked_add(1)?;
            let candidate = format!("group-{sequence:016x}");
            if !self.groups.iter().any(|group| group.id == candidate) {
                break candidate;
            }
        };
        self.groups.insert(
            0,
            SessionGroup {
                id: id.clone(),
                title,
                collapsed: false,
            },
        );
        Some(id)
    }

    pub fn rename(&mut self, id: &str, title: &str) -> bool {
        let Some(title) = clean_title(title) else {
            return false;
        };
        let Some(group) = self.groups.iter_mut().find(|group| group.id == id) else {
            return false;
        };
        if group.title == title {
            return false;
        }
        group.title = title;
        true
    }

    /// Move a group within the manually ordered list. Out-of-range positions
    /// are clamped to the last slot.
    pub fn move_group(&mut self, id: &str, position: usize) -> bool {
        let Some(previous) = self.groups.iter().position(|group| group.id == id) else {
            return false;
        };
        let next = position.min(self.groups.len() - 1);
        if previous == next {
            return false;
        }
        let group = self.groups.remove(previous);
        self.groups.insert(next, group);
        true
    }

    /// Delete a group and return its members to the ungrouped root.
    pub fn delete(&mut self, id: &str) -> bool {
        let Some(index) = self.groups.iter().position(|group| group.id == id) else {
            return false;
        };
        self.groups.remove(index);
        self.members.retain(|_, group_id| group_id != id);
        true
    }

    /// Replace any previous membership. Returns false for unknown groups or
    /// malformed session ids, and when the session already belongs to `id`.
    pub fn assign(&mut self, session_id: &str, id: &str) -> bool {
        if !valid_session_id(session_id)
            || !self.groups.iter().any(|group| group.id == id)
            || (!self.members.contains_key(session_id) && self.members.len() >= MAX_MEMBERS)
            || self
                .members
                .get(session_id)
                .is_some_and(|current| current == id)
        {
            return false;
        }
        self.members.insert(session_id.to_owned(), id.to_owned());
        true
    }

    pub fn unassign(&mut self, session_id: &str) -> bool {
        self.members.remove(session_id).is_some()
    }

    pub fn group_for(&self, session_id: &str) -> Option<&SessionGroup> {
        let id = self.members.get(session_id)?;
        self.groups.iter().find(|group| &group.id == id)
    }

    pub fn toggle_collapsed(&mut self, id: &str) -> bool {
        let Some(group) = self.groups.iter_mut().find(|group| group.id == id) else {
            return false;
        };
        group.collapsed = !group.collapsed;
        true
    }

    /// Set all group states; returns whether anything changed.
    pub fn collapse_all(&mut self, collapsed: bool) -> bool {
        let mut changed = false;
        for group in &mut self.groups {
            if group.collapsed != collapsed {
                group.collapsed = collapsed;
                changed = true;
            }
        }
        changed
    }

    /// Forget memberships of sessions known to have been deleted. Call this
    /// only with a complete session listing, never with a paginated view.
    pub fn prune<I, S>(&mut self, valid_session_ids: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let valid: BTreeSet<String> = valid_session_ids
            .into_iter()
            .map(|id| id.as_ref().to_owned())
            .collect();
        let before = self.members.len();
        self.members
            .retain(|session_id, _| valid.contains(session_id));
        self.members.len() != before
    }

    /// Sanitize data loaded from disk or constructed programmatically.
    pub fn validate(&mut self) {
        let mut ids = BTreeSet::new();
        self.groups.retain_mut(|group| {
            let Some(title) = clean_title(&group.title) else {
                return false;
            };
            if parse_group_id(&group.id).is_none() || !ids.insert(group.id.clone()) {
                return false;
            }
            group.title = title;
            true
        });
        self.groups.truncate(MAX_GROUPS);
        let valid_ids: BTreeSet<_> = self.groups.iter().map(|group| group.id.as_str()).collect();
        self.members.retain(|session_id, group_id| {
            valid_session_id(session_id) && valid_ids.contains(group_id.as_str())
        });
        while self.members.len() > MAX_MEMBERS {
            self.members.pop_last();
        }
        self.next_id = self
            .groups
            .iter()
            .filter_map(|group| parse_group_id(&group.id))
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(self.next_id)
            .max(1);
    }
}

fn clean_title(value: &str) -> Option<String> {
    let title = value.trim();
    if title.is_empty()
        || title.chars().count() > MAX_TITLE_CHARS
        || title.chars().any(char::is_control)
    {
        return None;
    }
    Some(title.to_owned())
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_SESSION_ID_BYTES && !value.chars().any(char::is_control)
}

fn parse_group_id(value: &str) -> Option<u64> {
    let hex = value.strip_prefix("group-")?;
    if hex.len() != 16 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_is_unique_and_deleting_a_group_preserves_sessions() {
        let mut groups = SessionGroups::default();
        let alpha = groups.create(" Alpha ").unwrap();
        let beta = groups.create("Beta").unwrap();
        assert_eq!(groups.groups[0].id, beta);
        assert!(groups.assign("session-a", &alpha));
        assert!(groups.assign("session-a", &beta));
        assert!(!groups.assign("session-a", &beta));
        assert_eq!(groups.group_for("session-a").unwrap().title, "Beta");
        assert!(groups.delete(&beta));
        assert!(groups.group_for("session-a").is_none());
        assert!(groups.groups.iter().any(|group| group.id == alpha));
    }

    #[test]
    fn persisted_order_collapsed_state_and_membership_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let mut groups = SessionGroups::default();
        let first = groups.create("Primero").unwrap();
        let second = groups.create("Segundo").unwrap();
        assert!(groups.assign("session-1", &first));
        assert!(groups.toggle_collapsed(&first));
        assert!(groups.save(root.path()).is_ok());
        let mut loaded = SessionGroups::load(root.path());
        assert_eq!(loaded, groups);
        assert_eq!(loaded.groups[0].id, second);
        assert!(loaded.group_for("session-1").unwrap().collapsed);
        assert!(loaded.move_group(&first, 0));
        assert_eq!(loaded.groups[0].id, first);
        assert!(!loaded.move_group(&first, 0));
        let third = loaded.create("Tercero").unwrap();
        assert_ne!(third, first);
        assert_ne!(third, second);
    }

    #[test]
    fn invalid_records_are_removed_without_losing_valid_groups() {
        let value = json!({
            "version": 1,
            "next_id": 1,
            "groups": [
                {"id":"group-0000000000000002","title":" Good ","collapsed":true},
                {"id":"group-0000000000000002","title":"Duplicate"},
                {"id":"../../bad","title":"Bad"},
                {"id":"group-0000000000000003","title":"\n"}
            ],
            "members": {"session-1":"group-0000000000000002", "session-2":"missing", "\n":"group-0000000000000002"}
        });
        let mut groups = SessionGroups::from_value(&value);
        assert_eq!(groups.groups.len(), 1);
        assert_eq!(groups.groups[0].title, "Good");
        assert_eq!(groups.members.len(), 1);
        assert_eq!(groups.group_for("session-1").unwrap().title, "Good");
        assert!(groups.prune(["session-3"]));
        assert!(groups.members.is_empty());
        assert_ne!(groups.create("New").unwrap(), "group-0000000000000002");
    }

    #[test]
    fn collapse_all_and_rename_reject_invalid_names() {
        let mut groups = SessionGroups::default();
        let first = groups.create("Primero").unwrap();
        let second = groups.create("Segundo").unwrap();
        assert!(groups.collapse_all(true));
        assert!(!groups.collapse_all(true));
        assert!(groups.groups.iter().all(|group| group.collapsed));
        assert!(!groups.rename(&first, " \n "));
        assert!(groups.rename(&second, " Nuevo "));
        assert_eq!(groups.groups[0].title, "Nuevo");
    }
}
