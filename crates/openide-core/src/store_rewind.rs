//! Rewind a native journal to before one admitted turn. The removed tail is
//! kept beside the journal so the host can put it back while nothing new has
//! been appended.

use super::*;

/// Why a rewind was not attempted. Nothing was changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRewindRefusal {
    /// The sequence is not the admission of a turn of this session.
    NotATurnStart,
    /// The last turn has no terminal event: it runs or awaits recovery.
    UnfinishedTurn,
    PendingSteers,
    PendingPermission,
    PendingCommand,
    /// A subagent's journal belongs to its parent's turn.
    SubagentSession,
    AutomationSession,
    /// A removed turn spawned a subagent whose session would outlive it.
    SpawnedSubagents,
    /// The journal no longer ends where the rewind left it.
    JournalChanged,
    /// The saved tail is missing or is not the one this rewind removed.
    TailUnavailable,
}

impl std::fmt::Display for NativeRewindRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotATurnStart => "the sequence is not the start of a turn",
            Self::UnfinishedTurn => "the last turn has not finished",
            Self::PendingSteers => "a steering message is still queued",
            Self::PendingPermission => "a permission request is still open",
            Self::PendingCommand => "a command operation is still pending",
            Self::SubagentSession => "the session belongs to a subagent",
            Self::AutomationSession => "the session belongs to an automation",
            Self::SpawnedSubagents => "a later turn spawned a subagent",
            Self::JournalChanged => "the journal changed after the rewind",
            Self::TailUnavailable => "the removed events are no longer available",
        })
    }
}

/// What [`SessionStore::rewind_native`] removed.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeRewind {
    pub session_id: String,
    pub before_sequence: u64,
    /// The removed events, oldest first. The same records are in `tail_path`.
    pub removed: Vec<AgentEvent>,
    pub tail_path: PathBuf,
    /// Byte length of the journal the rewind left.
    prefix_len: u64,
    steers: Vec<NativeSteerInput>,
    finishing_turn_id: Option<String>,
    permissions: Vec<RecordedPermissionDecision>,
}

#[cfg(test)]
thread_local! {
    /// Fail after the temporary prefix is synced and before it replaces the
    /// journal, as a crash at that point would.
    static FAIL_BEFORE_PUBLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn refused(reason: NativeRewindRefusal) -> StoreError {
    StoreError::NativeRewindRefused(reason)
}

fn turn_ids(events: &[AgentEvent]) -> HashSet<&str> {
    events
        .iter()
        .filter_map(|event| native_admission_fields(event).map(|fields| fields.0))
        .collect()
}

impl SessionStore {
    fn rewind_tail_prefix(session_id: &str) -> String {
        format!("{session_id}.rewind-")
    }

    /// Create `path` with `bytes`, synced. Fails when it already exists.
    fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }

    /// Replace the journal with `bytes` through a synced temporary and a
    /// rename, so a crash leaves either the old journal or the new one.
    fn publish_journal(
        &self,
        session_id: &str,
        bytes: &[u8],
        journal: &mut NativeJournalTransaction,
    ) -> Result<(), StoreError> {
        let path = self.events.path(session_id)?;
        let temporary = self
            .events
            .root
            .join(format!(".{session_id}.rewind.{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<(), StoreError> {
            Self::write_new_synced(&temporary, bytes)?;
            #[cfg(test)]
            if FAIL_BEFORE_PUBLISH.with(|fail| fail.get()) {
                return Err(StoreError::Io(std::io::Error::other("injected failure")));
            }
            // A cached tail of the old journal must not match the new one,
            // even if a later append brings it back to the same length.
            journal.bump_generation()?;
            fs::rename(&temporary, &path)?;
            #[cfg(unix)]
            File::open(&self.events.root)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        self.native_tail
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?
            .remove(session_id);
        self.events
            .write_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?
            .remove(session_id);
        result
    }

    /// Remove the turn admitted at `before_sequence` and everything after it.
    /// The session must be idle: the native lease is taken for the duration,
    /// and an unfinished turn, a queued steer, an open permission request or
    /// a pending command refuses the rewind. The removed records are saved
    /// first, to `{id}.rewind-<ms>.jsonl` beside the journal.
    pub fn rewind_native(
        &self,
        session_id: &str,
        before_sequence: u64,
    ) -> Result<NativeRewind, StoreError> {
        validate_session_id(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut journal = self.lock_native_journal(session_id)?;
        let _lease = self.try_acquire_native_lease_locked(session_id)?;
        let session = self
            .read_metadata(session_id)?
            .ok_or_else(|| StoreError::UnknownSession(session_id.to_owned()))?;
        if session.subagent.is_some() {
            return Err(refused(NativeRewindRefusal::SubagentSession));
        }
        if session.native_automation.is_some() {
            return Err(refused(NativeRewindRefusal::AutomationSession));
        }
        let events = self.events.load(session_id)?;
        let cut = events
            .iter()
            .position(|event| event.sequence == before_sequence)
            .filter(|index| native_admission_fields(&events[*index]).is_some())
            .ok_or_else(|| refused(NativeRewindRefusal::NotATurnStart))?;
        // A turn left unfinished is the last one, so the cut removes it whole.
        // The lease above already rules out a worker still running it.
        let mut open_permissions = HashSet::new();
        for event in &events {
            match &event.kind {
                AgentEventKind::PermissionRequested { request_id, .. } => {
                    open_permissions.insert(request_id.as_str());
                }
                AgentEventKind::PermissionResolved { request_id, .. } => {
                    open_permissions.remove(request_id.as_str());
                }
                _ => {}
            }
        }
        if !open_permissions.is_empty() {
            return Err(refused(NativeRewindRefusal::PendingPermission));
        }
        let mut inbox = self.read_native_steer_inbox(session_id)?;
        if !pending_native_steers_from(&inbox, &committed_native_steers(&events)?)?.is_empty() {
            return Err(refused(NativeRewindRefusal::PendingSteers));
        }
        let _command_guard = crate::native_command_operation::lock_and_require_no_pending(
            self.storage_root(),
            &session,
        )
        .map_err(|_| refused(NativeRewindRefusal::PendingCommand))?;
        let _command_output_guard =
            crate::native_command_output::lock_and_validate(self.storage_root(), &session)
                .map_err(|error| StoreError::NativeCommandOutputBlocked(error.to_string()))?;
        let (kept, removed) = events.split_at(cut);
        // A subagent's session and registry entry outlive the call that
        // created or fed it, and nothing here can remove one child. The
        // creation time also catches a child whose call never reached the
        // journal.
        let subagent_call = removed.iter().any(|event| {
            matches!(
                &event.kind,
                AgentEventKind::ToolStarted { tool_call }
                    if matches!(tool_call.name.as_str(), "spawn_agent" | "send_input")
            )
        });
        if subagent_call
            || self.scan_session_metadata()?.iter().any(|child| {
                child.created_at_ms >= removed[0].timestamp_ms
                    && child
                        .subagent
                        .as_ref()
                        .is_some_and(|link| link.parent_session_id == session_id)
            })
        {
            return Err(refused(NativeRewindRefusal::SpawnedSubagents));
        }

        // `load` validated one record per line, so the cut is a line count.
        let bytes = fs::read(self.events.path(session_id)?)?;
        let mut offset = 0;
        for _ in 0..cut {
            offset += bytes[offset..]
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or_else(|| refused(NativeRewindRefusal::JournalChanged))?
                + 1;
        }
        let first_end = bytes[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |end| offset + end);
        if serde_json::from_slice::<AgentEvent>(&bytes[offset..first_end])? != removed[0] {
            return Err(refused(NativeRewindRefusal::JournalChanged));
        }
        let tail_path = self.events.root.join(format!(
            "{}{}-{}.jsonl",
            Self::rewind_tail_prefix(session_id),
            current_time_ms(),
            Uuid::new_v4().simple()
        ));
        Self::write_new_synced(&tail_path, &bytes[offset..])?;

        // Steers committed by a removed turn would read as queued once their
        // events are gone, and block every later admission.
        let removed_turns = turn_ids(removed);
        let steers: Vec<_> = inbox
            .entries
            .iter()
            .filter(|entry| removed_turns.contains(entry.turn_id.as_str()))
            .cloned()
            .collect();
        let finishing_turn_id = inbox
            .finishing_turn_id
            .clone()
            .filter(|turn_id| removed_turns.contains(turn_id.as_str()));
        // A decision, and any "allow for this session" grant it carries, is
        // recorded between its request and its resolution. One made in a
        // removed turn must not keep allowing calls.
        let removed_requests: HashSet<&str> = removed
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::PermissionRequested { request_id, .. } => Some(request_id.as_str()),
                _ => None,
            })
            .collect();
        let mut permission_journal = self.read_permission_journal(session_id)?;
        let (permissions, kept_permissions): (Vec<_>, Vec<_>) = permission_journal
            .entries
            .drain(..)
            .partition(|entry| removed_requests.contains(entry.request_id.as_str()));
        permission_journal.entries = kept_permissions;
        let published = (|| -> Result<(), StoreError> {
            if !permissions.is_empty() {
                self.write_permission_journal_atomically(session_id, &permission_journal)?;
            }
            if !steers.is_empty() || finishing_turn_id.is_some() {
                inbox
                    .entries
                    .retain(|entry| !removed_turns.contains(entry.turn_id.as_str()));
                if finishing_turn_id.is_some() {
                    inbox.finishing_turn_id = None;
                }
                self.write_native_steer_inbox(session_id, &inbox)?;
            }
            self.publish_journal(session_id, &bytes[..offset], &mut journal)
        })();
        if let Err(error) = published {
            // The journal is whole: put its sidecars back and drop the copy.
            if !permissions.is_empty() {
                permission_journal.entries.extend(permissions);
                let _ = self.write_permission_journal_atomically(session_id, &permission_journal);
            }
            if !steers.is_empty() || finishing_turn_id.is_some() {
                inbox.entries.extend(steers);
                inbox.finishing_turn_id = inbox.finishing_turn_id.or(finishing_turn_id);
                let _ = self.write_native_steer_inbox(session_id, &inbox);
            }
            let _ = fs::remove_file(&tail_path);
            return Err(error);
        }
        debug_assert_eq!(kept.len(), cut);
        Ok(NativeRewind {
            session_id: session_id.to_owned(),
            before_sequence,
            removed: removed.to_vec(),
            tail_path,
            prefix_len: offset as u64,
            steers,
            finishing_turn_id,
            permissions,
        })
    }

    /// Whether [`Self::undo_native_rewind`] would still put the tail back.
    pub fn can_undo_native_rewind(&self, rewind: &NativeRewind) -> bool {
        self.native_journal_len(&rewind.session_id)
            .is_ok_and(|len| len == rewind.prefix_len)
            && rewind.tail_path.is_file()
    }

    /// Put a rewound tail back. Only while the journal still ends exactly
    /// where the rewind left it; a single appended event refuses the undo.
    pub fn undo_native_rewind(&self, rewind: &NativeRewind) -> Result<(), StoreError> {
        let session_id = rewind.session_id.as_str();
        validate_session_id(session_id)?;
        let _guard = self
            .operation_lock
            .lock()
            .map_err(|_| StoreError::PoisonedLock)?;
        let mut journal = self.lock_native_journal(session_id)?;
        let _lease = self.try_acquire_native_lease_locked(session_id)?;
        let mut bytes = fs::read(self.events.path(session_id)?)?;
        let kept = self.events.load(session_id)?;
        if bytes.len() as u64 != rewind.prefix_len
            || kept
                .last()
                .is_some_and(|last| last.sequence.checked_add(1) != Some(rewind.before_sequence))
        {
            return Err(refused(NativeRewindRefusal::JournalChanged));
        }
        let tail = match fs::read(&rewind.tail_path) {
            Ok(tail) => tail,
            Err(_) => return Err(refused(NativeRewindRefusal::TailUnavailable)),
        };
        let saved: Result<Vec<AgentEvent>, _> = tail
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<AgentEvent>)
            .collect();
        if saved.ok().as_deref() != Some(rewind.removed.as_slice()) {
            return Err(refused(NativeRewindRefusal::TailUnavailable));
        }
        if !rewind.steers.is_empty() || rewind.finishing_turn_id.is_some() {
            let mut inbox = self.read_native_steer_inbox(session_id)?;
            for steer in &rewind.steers {
                if !inbox.entries.iter().any(|entry| entry.input_id == steer.input_id) {
                    inbox.entries.push(steer.clone());
                }
            }
            inbox.finishing_turn_id = inbox
                .finishing_turn_id
                .or_else(|| rewind.finishing_turn_id.clone());
            self.write_native_steer_inbox(session_id, &inbox)?;
        }
        if !rewind.permissions.is_empty() {
            let mut journal = self.read_permission_journal(session_id)?;
            for decision in &rewind.permissions {
                if !journal.entries.iter().any(|entry| entry.request_id == decision.request_id) {
                    journal.entries.push(decision.clone());
                }
            }
            self.write_permission_journal_atomically(session_id, &journal)?;
        }
        bytes.extend_from_slice(&tail);
        if bytes.last() != Some(&b'\n') {
            bytes.push(b'\n');
        }
        self.publish_journal(session_id, &bytes, &mut journal)?;
        let _ = fs::remove_file(&rewind.tail_path);
        Ok(())
    }

    /// Remove the tails rewinds saved for a session that is being deleted.
    pub(super) fn remove_rewind_tails(&self, session_id: &str) -> Result<bool, StoreError> {
        let prefix = Self::rewind_tail_prefix(session_id);
        let mut removed = false;
        for entry in fs::read_dir(&self.events.root)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".jsonl"))
            {
                removed |= remove_if_exists(entry.path())?;
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(store: &SessionStore, session_id: &str, turn_id: &str, prompt: &str) -> u64 {
        let admitted = store
            .admit_native_turn(session_id, turn_id, prompt, "model-a", TurnMode::Build)
            .unwrap();
        let mut sequence = admitted.sequence;
        for kind in [
            AgentEventKind::TurnStarted { prompt: prompt.into() },
            AgentEventKind::AssistantMessage { content: format!("Answer to {prompt}") },
            AgentEventKind::TurnCompleted { response: format!("Answer to {prompt}"), steps: 1 },
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(session_id, sequence, kind))
                .unwrap();
        }
        admitted.sequence
    }

    fn events(store: &SessionStore, session_id: &str) -> Vec<AgentEvent> {
        store.load(session_id).unwrap().unwrap().events
    }

    fn fixture() -> (tempfile::TempDir, SessionStore, String, [u64; 3]) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let cuts = ["one", "two", "three"].map(|name| turn(&store, &session.id, name, name));
        (dir, store, session.id, cuts)
    }

    #[test]
    fn rewound_prefix_is_the_journal_of_a_session_that_never_had_the_tail() {
        let (dir, store, id, cuts) = fixture();
        let before = events(&store, &id);
        let rewind = store.rewind_native(&id, cuts[1]).unwrap();

        // A second session given only the first turn, with the same identity.
        let twin_dir = tempfile::tempdir().unwrap();
        fs::copy(
            dir.path().join(format!("{id}.session.json")),
            twin_dir.path().join(format!("{id}.session.json")),
        )
        .unwrap();
        let twin = SessionStore::open(twin_dir.path()).unwrap();
        for event in &before[..4] {
            twin.events.append(event).unwrap();
        }
        assert_eq!(
            fs::read(dir.path().join(format!("{id}.jsonl"))).unwrap(),
            fs::read(twin_dir.path().join(format!("{id}.jsonl"))).unwrap()
        );
        assert_eq!(events(&store, &id), before[..4]);
        // Byte-identical journals replay identically.

        // The tail file holds exactly the removed events.
        assert_eq!(rewind.removed, before[4..]);
        let saved: Vec<AgentEvent> = fs::read_to_string(&rewind.tail_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(saved, before[4..]);
        // It is not listed as a session.
        assert_eq!(store.events.list_sessions().unwrap(), vec![id.clone()]);

        // The next turn reuses the removed sequences, from this handle and
        // from one opened before the rewind.
        assert_eq!(turn(&store, &id, "again", "again"), cuts[1]);
        assert_eq!(events(&store, &id).len(), 8);
    }

    #[test]
    fn a_handle_with_a_cached_tail_sees_the_rewind() {
        let (dir, store, id, cuts) = fixture();
        let other = SessionStore::open(dir.path()).unwrap();
        // Prime the other handle's cached tail with the full journal.
        let last = events(&other, &id).last().unwrap().clone();
        other.append_native_event(&last).unwrap();
        store.rewind_native(&id, cuts[2]).unwrap();
        assert_eq!(turn(&other, &id, "again", "again"), cuts[2]);
        assert_eq!(events(&store, &id).len(), 12);
    }

    #[test]
    fn rewind_refuses_what_it_cannot_do_safely_and_changes_nothing() {
        let (dir, store, id, cuts) = fixture();
        let journal = dir.path().join(format!("{id}.jsonl"));
        let before = fs::read(&journal).unwrap();
        let refusal = |result: Result<NativeRewind, StoreError>| match result {
            Err(StoreError::NativeRewindRefused(reason)) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        };

        // Not the admission of a turn.
        assert_eq!(refusal(store.rewind_native(&id, cuts[1] + 1)), NativeRewindRefusal::NotATurnStart);
        assert_eq!(refusal(store.rewind_native(&id, 999)), NativeRewindRefusal::NotATurnStart);

        // A worker holds the session.
        let lease = store.try_acquire_native_lease(&id).unwrap();
        assert!(matches!(
            store.rewind_native(&id, cuts[1]),
            Err(StoreError::NativeSessionBusy(_))
        ));
        drop(lease);

        // An unfinished turn, with a queued steer and an open permission.
        let admitted = store
            .admit_native_turn(&id, "open", "open", "model-a", TurnMode::Build)
            .unwrap();
        // An interrupted turn is rewound with the rest: nothing else settles it.
        let _ = admitted;
        assert_ne!(fs::read(&journal).unwrap(), before);
        assert!(store.rewind_native(&id, cuts[0]).is_ok());
        assert_eq!(store.active_native_turn_id(&id).unwrap(), None);

        // A subagent's own journal is never rewound.
        let (_dir, store, id, cuts) = fixture();
        let mut session = store.read_metadata(&id).unwrap().unwrap();
        session.subagent = Some(SubagentLink {
            parent_session_id: "parent".into(),
            depth: 1,
            spawn_id: "call".into(),
            request_digest: String::new(),
        });
        store.write_metadata_atomically(&session).unwrap();
        assert_eq!(refusal(store.rewind_native(&id, cuts[1])), NativeRewindRefusal::SubagentSession);
        assert_eq!(events(&store, &id).len(), 12);
        assert!(!fs::read_dir(_dir.path())
            .unwrap()
            .any(|entry| entry.unwrap().file_name().to_string_lossy().contains(".rewind")));
    }

    #[test]
    fn turns_that_reached_a_subagent_are_not_rewound() {
        for tool in ["spawn_agent", "send_input"] {
            let (_dir, store, id, cuts) = fixture();
            let next = events(&store, &id).last().unwrap().sequence + 1;
            store
                .admit_native_turn(&id, "spawn", "spawn", "model-a", TurnMode::Build)
                .unwrap();
            let call = crate::ToolCall {
                id: "call-1".into(),
                name: tool.into(),
                arguments: serde_json::json!({}),
            };
            for (offset, kind) in [
                AgentEventKind::TurnStarted { prompt: "spawn".into() },
                AgentEventKind::ToolStarted { tool_call: call },
                AgentEventKind::ToolFinished { tool_call_id: "call-1".into(), output: "ok".into(), is_error: false },
                AgentEventKind::TurnCompleted { response: "ok".into(), steps: 1 },
            ]
            .into_iter()
            .enumerate()
            {
                store
                    .append_native_event(&AgentEvent::new(&id, next + 1 + offset as u64, kind))
                    .unwrap();
            }
            for cut in [cuts[0], next] {
                assert!(matches!(
                    store.rewind_native(&id, cut),
                    Err(StoreError::NativeRewindRefused(NativeRewindRefusal::SpawnedSubagents))
                ));
            }
            assert_eq!(events(&store, &id).len(), 17);
        }

        // A child created after the cut, whatever the journal says.
        let (_dir, store, id, cuts) = fixture();
        let mut child = store.create("/workspace", "native").unwrap();
        child.subagent = Some(SubagentLink {
            parent_session_id: id.clone(),
            depth: 1,
            spawn_id: "tool-1".into(),
            request_digest: String::new(),
        });
        store.write_metadata_atomically(&child).unwrap();
        assert!(matches!(
            store.rewind_native(&id, cuts[0]),
            Err(StoreError::NativeRewindRefused(NativeRewindRefusal::SpawnedSubagents))
        ));
    }

    #[test]
    fn decisions_of_removed_turns_stop_allowing_and_return_with_the_undo() {
        let (_dir, store, id, cuts) = fixture();
        let next = events(&store, &id).last().unwrap().sequence + 1;
        store
            .admit_native_turn(&id, "ask", "ask", "model-a", TurnMode::Build)
            .unwrap();
        let call = crate::ToolCall {
            id: "call-1".into(),
            name: "shell".into(),
            arguments: serde_json::json!({}),
        };
        let append = |offset: u64, kind| {
            store
                .append_native_event(&AgentEvent::new(&id, next + offset, kind))
                .unwrap()
        };
        append(1, AgentEventKind::TurnStarted { prompt: "ask".into() });
        append(2, AgentEventKind::ToolStarted { tool_call: call.clone() });
        append(3, AgentEventKind::PermissionRequested { request_id: "request-1".into(), tool_call: call });
        store
            .record_permission_decision(&id, "request-1", PermissionDecision::Allow)
            .unwrap();
        append(
            4,
            AgentEventKind::PermissionResolved {
                request_id: "request-1".into(),
                tool_call_id: "call-1".into(),
                resolution: crate::PermissionResolution::Allowed,
            },
        );
        append(5, AgentEventKind::ToolFinished { tool_call_id: "call-1".into(), output: "ok".into(), is_error: false });
        append(6, AgentEventKind::TurnCompleted { response: "ok".into(), steps: 1 });
        assert_eq!(store.load_permission_decisions(&id).unwrap().len(), 1);

        // A cut after the turn that asked keeps its decision.
        let later = turn(&store, &id, "later", "later");
        let rewind = store.rewind_native(&id, later).unwrap();
        assert_eq!(store.load_permission_decisions(&id).unwrap().len(), 1);
        store.undo_native_rewind(&rewind).unwrap();
        // A cut at or before it removes the decision, and the undo returns it.
        for cut in [next, cuts[2]] {
            let rewind = store.rewind_native(&id, cut).unwrap();
            assert!(store.load_permission_decisions(&id).unwrap().is_empty());
            store.undo_native_rewind(&rewind).unwrap();
            assert_eq!(store.load_permission_decisions(&id).unwrap().len(), 1);
        }
    }

    #[test]
    fn failure_before_the_prefix_replaces_the_journal_leaves_it_intact() {
        let (dir, store, id, cuts) = fixture();
        let journal = dir.path().join(format!("{id}.jsonl"));
        let before = fs::read(&journal).unwrap();
        FAIL_BEFORE_PUBLISH.with(|fail| fail.set(true));
        let result = store.rewind_native(&id, cuts[1]);
        FAIL_BEFORE_PUBLISH.with(|fail| fail.set(false));
        assert!(matches!(result, Err(StoreError::Io(_))));
        assert_eq!(fs::read(&journal).unwrap(), before);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|name| !name.contains("rewind")), "{names:?}");
        // The session still works, and a later rewind succeeds.
        assert_eq!(events(&store, &id).len(), 12);
        assert!(store.rewind_native(&id, cuts[1]).is_ok());
    }

    #[test]
    fn undo_puts_the_tail_back_only_while_nothing_was_appended() {
        let (dir, store, id, cuts) = fixture();
        let journal = dir.path().join(format!("{id}.jsonl"));
        let before = fs::read(&journal).unwrap();
        let rewind = store.rewind_native(&id, cuts[1]).unwrap();
        assert!(store.can_undo_native_rewind(&rewind));
        store.undo_native_rewind(&rewind).unwrap();
        assert_eq!(fs::read(&journal).unwrap(), before);
        assert!(!rewind.tail_path.exists());
        assert!(!store.can_undo_native_rewind(&rewind));
        // The restored journal accepts the next turn.
        let next = turn(&store, &id, "four", "four");
        assert_eq!(next, events(&store, &id)[11].sequence + 1);

        // Once a new turn follows the rewind, the old tail stays out.
        let rewind = store.rewind_native(&id, cuts[2]).unwrap();
        turn(&store, &id, "other", "other");
        let after = fs::read(&journal).unwrap();
        assert!(!store.can_undo_native_rewind(&rewind));
        assert!(matches!(
            store.undo_native_rewind(&rewind),
            Err(StoreError::NativeRewindRefused(NativeRewindRefusal::JournalChanged))
        ));
        assert_eq!(fs::read(&journal).unwrap(), after);
        assert!(rewind.tail_path.exists(), "the removed events are still saved");

        // A tail that was tampered with is not appended.
        let rewind = store.rewind_native(&id, cuts[1]).unwrap();
        fs::write(&rewind.tail_path, b"{}\n").unwrap();
        let after = fs::read(&journal).unwrap();
        assert!(matches!(
            store.undo_native_rewind(&rewind),
            Err(StoreError::NativeRewindRefused(NativeRewindRefusal::TailUnavailable))
        ));
        assert_eq!(fs::read(&journal).unwrap(), after);
    }

    #[test]
    fn steers_of_removed_turns_leave_the_inbox_and_return_with_the_undo() {
        let (_dir, store, id, cuts) = fixture();
        // A steer committed by the last turn, as its inbox keeps it.
        let steer = NativeSteerInput {
            turn_id: "three".into(),
            input_id: "input-1".into(),
            text: "also this".into(),
            images: Vec::new(),
        };
        let mut inbox = store.read_native_steer_inbox(&id).unwrap();
        inbox.entries.push(steer.clone());
        inbox.finishing_turn_id = Some("three".into());
        store.write_native_steer_inbox(&id, &inbox).unwrap();
        // Uncommitted, it is a queued steer.
        assert!(matches!(
            store.rewind_native(&id, cuts[0]),
            Err(StoreError::NativeRewindRefused(NativeRewindRefusal::PendingSteers))
        ));
        // Committed in the journal, it belongs to the removed turn.
        let path = store.events.path(&id).unwrap();
        let mut all = events(&store, &id);
        let terminal = all.pop().unwrap();
        all.push(AgentEvent::new(
            &id,
            terminal.sequence,
            AgentEventKind::TurnSteered {
                turn_id: "three".into(),
                input_id: "input-1".into(),
                text: "also this".into(),
            },
        ));
        all.push(AgentEvent::new(&id, terminal.sequence + 1, AgentEventKind::TurnCancelled));
        let lines: String = all
            .iter()
            .map(|event| serde_json::to_string(event).unwrap() + "\n")
            .collect();
        fs::write(&path, lines).unwrap();
        let fresh = SessionStore::open(store.storage_root()).unwrap();
        let rewind = fresh.rewind_native(&id, cuts[2]).unwrap();
        let inbox = fresh.read_native_steer_inbox(&id).unwrap();
        assert!(inbox.entries.is_empty());
        assert_eq!(inbox.finishing_turn_id, None);
        // The next admission is not blocked by the removed steer.
        assert_eq!(turn(&fresh, &id, "again", "again"), cuts[2]);

        let (_dir, store, id, cuts) = fixture();
        let mut inbox = store.read_native_steer_inbox(&id).unwrap();
        inbox.finishing_turn_id = Some("three".into());
        store.write_native_steer_inbox(&id, &inbox).unwrap();
        let rewind_two = store.rewind_native(&id, cuts[2]).unwrap();
        assert_eq!(store.read_native_steer_inbox(&id).unwrap().finishing_turn_id, None);
        store.undo_native_rewind(&rewind_two).unwrap();
        assert_eq!(
            store.read_native_steer_inbox(&id).unwrap().finishing_turn_id.as_deref(),
            Some("three")
        );
        drop(rewind);
    }

    #[test]
    fn deleting_a_session_removes_its_saved_tails() {
        let (dir, store, id, cuts) = fixture();
        let rewind = store.rewind_native(&id, cuts[1]).unwrap();
        assert!(rewind.tail_path.exists());
        assert!(store.delete(&id).unwrap());
        assert!(!rewind.tail_path.exists());
        assert!(!fs::read_dir(dir.path())
            .unwrap()
            .any(|entry| entry.unwrap().file_name().to_string_lossy().contains(".rewind-")));
    }
}
