//! Durable decisions on immutable edits. Accept folds only the chosen block
//! into the review baseline. Reject restores only its exact original bytes.
use super::*;
use openide_ui::CodeReviewHunk;
use std::ops::Range;

const DECISIONS: &str = "session-file-review-decisions";
const MAX_STEPS: usize = 1024;
const MAX_RECORD_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct FileReviewBlock {
    pub id: String,
    pub hunk: CodeReviewHunk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum FileReviewDecision {
    Accept,
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileReviewDecisionTarget {
    File,
    Block(String),
}

pub(crate) struct FileReviewDecisionResult {
    pub review: FileReview,
    pub disk_changed: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decisions {
    blocks: BTreeMap<String, FileReviewDecision>,
    structural: Option<FileReviewDecision>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRecord {
    schema_version: u8,
    evidence_version: String,
    step: usize,
    previous: String,
    state: Decisions,
    disk_changed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDecision {
    sha256: String,
    payload: DecisionRecord,
}

struct OriginalBlock {
    id: String,
    hunk: CodeReviewHunk,
    old_bytes: Range<usize>,
    new_bytes: Range<usize>,
}

fn decision_version(review: &FileReview, decisions: &Decisions) -> io::Result<String> {
    Ok(digest(
        &serde_json::to_vec(&(review.version.as_str(), decisions)).map_err(io::Error::other)?,
    ))
}

fn offsets(text: &str) -> Vec<usize> {
    let mut result = vec![0];
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        result.push(offset);
    }
    result
}

fn original_blocks(review: &FileReview) -> io::Result<Vec<OriginalBlock>> {
    let before = review.before.as_deref().unwrap_or_default();
    let after = review.after.as_deref().unwrap_or_default();
    let old_offsets = offsets(before);
    let new_offsets = offsets(after);
    let hunks = crate::editor_review_projection::build_review_hunks(&review.original_file);
    let mut result = Vec::with_capacity(hunks.len());
    let (mut old_cursor, mut new_cursor) = (0, 0);
    for hunk in hunks.iter() {
        let old_end = hunk
            .old_start_row
            .checked_add(hunk.removed_lines.len())
            .ok_or_else(|| invalid("Rango de bloque inválido"))?;
        let old_bytes = *old_offsets
            .get(hunk.old_start_row)
            .ok_or_else(|| invalid("Falta el inicio original del bloque"))?
            ..*old_offsets
                .get(old_end)
                .ok_or_else(|| invalid("Falta el final original del bloque"))?;
        let new_bytes = *new_offsets
            .get(hunk.new_rows.start)
            .ok_or_else(|| invalid("Falta el inicio modificado del bloque"))?
            ..*new_offsets
                .get(hunk.new_rows.end)
                .ok_or_else(|| invalid("Falta el final modificado del bloque"))?;
        if old_bytes.start < old_cursor
            || new_bytes.start < new_cursor
            || before.get(old_cursor..old_bytes.start) != after.get(new_cursor..new_bytes.start)
        {
            return Err(invalid(
                "Los bloques no cubren exactamente las versiones registradas",
            ));
        }
        let id = digest(
            &serde_json::to_vec(&(
                &review.version,
                old_bytes.start,
                old_bytes.end,
                new_bytes.start,
                new_bytes.end,
                &before[old_bytes.clone()],
                &after[new_bytes.clone()],
            ))
            .map_err(io::Error::other)?,
        );
        old_cursor = old_bytes.end;
        new_cursor = new_bytes.end;
        result.push(OriginalBlock {
            id,
            hunk: hunk.clone(),
            old_bytes,
            new_bytes,
        });
    }
    if before.get(old_cursor..) != after.get(new_cursor..) {
        return Err(invalid(
            "El diff no conserva todos los bytes necesarios para decidir por bloque",
        ));
    }
    Ok(result)
}

/// Assemble from original slices, including exact CRLF and the final newline.
/// No displayed/truncated diff string is ever written into the workspace.
fn contents(
    review: &FileReview,
    blocks: &[OriginalBlock],
    state: &Decisions,
    baseline: bool,
) -> io::Result<Option<Arc<str>>> {
    let before = review.before.as_deref().unwrap_or_default();
    let after = review.after.as_deref().unwrap_or_default();
    let source = if baseline { before } else { after };
    let mut text = String::with_capacity(source.len());
    let mut cursor = 0;
    for block in blocks {
        let range = if baseline {
            &block.old_bytes
        } else {
            &block.new_bytes
        };
        text.push_str(&source[cursor..range.start]);
        let substitute = if baseline {
            state.blocks.get(&block.id) == Some(&FileReviewDecision::Accept)
        } else {
            state.blocks.get(&block.id) == Some(&FileReviewDecision::Reject)
        };
        if substitute {
            text.push_str(if baseline {
                &after[block.new_bytes.clone()]
            } else {
                &before[block.old_bytes.clone()]
            });
        } else {
            text.push_str(&source[range.clone()]);
        }
        cursor = range.end;
    }
    text.push_str(&source[cursor..]);
    if text.len() > MAX_FILE_BYTES {
        return Err(invalid("La versión revisada supera el límite del archivo"));
    }
    let all = |decision| {
        if blocks.is_empty() {
            state.structural == Some(decision)
        } else {
            blocks
                .iter()
                .all(|block| state.blocks.get(&block.id) == Some(&decision))
        }
    };
    let exists = if baseline {
        if all(FileReviewDecision::Accept) {
            review.after.is_some()
        } else {
            review.before.is_some() || !text.is_empty()
        }
    } else if all(FileReviewDecision::Reject) {
        review.before.is_some()
    } else {
        review.after.is_some() || !text.is_empty()
    };
    Ok(exists.then(|| Arc::from(text)))
}

fn structural(review: &FileReview, blocks: &[OriginalBlock]) -> bool {
    blocks.is_empty() || review.old_path.is_some()
}

fn validate_state(
    review: &FileReview,
    blocks: &[OriginalBlock],
    state: &Decisions,
) -> io::Result<()> {
    if state.blocks.len() > blocks.len()
        || state
            .blocks
            .keys()
            .any(|id| !blocks.iter().any(|block| &block.id == id))
        || (state.structural.is_some() && !structural(review, blocks))
        || (review.old_path.is_some()
            && (state.structural == Some(FileReviewDecision::Reject)
                || state
                    .blocks
                    .values()
                    .any(|decision| *decision == FileReviewDecision::Reject)))
    {
        return Err(invalid(
            "Las decisiones no corresponden a los bloques registrados",
        ));
    }
    Ok(())
}

fn name(review: &FileReview, step: usize, pending: bool) -> String {
    format!(
        "{}.{}.{step:04}{}.json",
        review.session_id,
        review.version,
        if pending { ".pending" } else { "" }
    )
}

fn read_record(directory: &Directory, name: &str) -> io::Result<Option<DecisionRecord>> {
    let file = match directory.read(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    require_private_file(&metadata)?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES as u64 {
        return Err(invalid("El registro de decisiones no es válido"));
    }
    let mut bytes = Vec::new();
    file.take((MAX_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(invalid("El registro de decisiones es demasiado grande"));
    }
    let record: StoredDecision = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if record.sha256 != digest(&serde_json::to_vec(&record.payload).map_err(io::Error::other)?) {
        return Err(invalid("El registro de decisiones está dañado"));
    }
    Ok(Some(record.payload))
}

fn load_state(
    storage: &Path,
    review: &FileReview,
    blocks: &[OriginalBlock],
) -> io::Result<(Decisions, usize)> {
    let mut state = Decisions::default();
    let Some(directory) = Directory::open_named(storage, DECISIONS, false)? else {
        return Ok((state, 0));
    };
    for step in 1..=MAX_STEPS {
        let committed = name(review, step, false);
        let pending = name(review, step, true);
        let (record, recovering) = if let Some(record) = read_record(&directory, &committed)? {
            (record, false)
        } else if let Some(record) = read_record(&directory, &pending)? {
            (record, true)
        } else {
            let prefix = format!("{}.{}.", review.session_id, review.version);
            for entry in fs::read_dir(storage.join(DECISIONS))?.take(MAX_RECEIPTS + 1) {
                let filename = entry?.file_name();
                let Some(rest) = filename
                    .to_str()
                    .and_then(|name| name.strip_prefix(&prefix))
                else {
                    continue;
                };
                let index = rest
                    .split('.')
                    .next()
                    .and_then(|index| index.parse::<usize>().ok());
                if index.is_some_and(|index| index >= step) {
                    return Err(invalid(
                        "Falta una decisión anterior en el registro de esta revisión",
                    ));
                }
            }
            return Ok((state, step - 1));
        };
        validate_state(review, blocks, &record.state)?;
        if record.schema_version != 1
            || record.evidence_version != review.version
            || record.step != step
            || record.previous != decision_version(review, &state)?
            || state
                .blocks
                .iter()
                .any(|(id, decision)| record.state.blocks.get(id) != Some(decision))
            || (state.structural.is_some() && state.structural != record.state.structural)
            || state == record.state
        {
            return Err(invalid(
                "La cadena de decisiones no corresponde a esta revisión",
            ));
        }
        if recovering {
            if !record.disk_changed {
                return Err(invalid("Preparación de revisión no válida"));
            }
            let previous = contents(review, blocks, &state, false)?;
            let next = contents(review, blocks, &record.state, false)?;
            if disk_mismatch(
                &review.workspace,
                &review.path,
                next.as_deref().map(str::as_bytes),
            )
            .is_none()
            {
                // Recovery only publishes metadata about already-observed bytes.
                // It never retries a workspace write while browsing history.
                directory.publish(&pending, &committed)?;
                directory.sync()?;
                directory.remove(&pending)?;
                directory.sync()?;
            } else if disk_mismatch(
                &review.workspace,
                &review.path,
                previous.as_deref().map(str::as_bytes),
            )
            .is_none()
            {
                directory.remove(&pending)?;
                directory.sync()?;
                return Ok((state, step - 1));
            } else {
                return Err(invalid(
                    "Hay una decisión interrumpida y el archivo tiene cambios externos; conserva esos cambios antes de reabrir la revisión",
                ));
            }
        }
        state = record.state;
    }
    Err(invalid("Esta revisión alcanzó el límite de decisiones"))
}

pub(super) fn project(storage: &Path, review: &mut FileReview) -> io::Result<()> {
    let blocks = match original_blocks(review) {
        Ok(blocks) => blocks,
        Err(error) => {
            review.can_accept = false;
            review.acceptance_unavailable = Some(error.to_string());
            review.can_reject = false;
            review.rejection_unavailable = Some(error.to_string());
            review.decision_version = decision_version(review, &Decisions::default())?;
            return Ok(());
        }
    };
    let (state, _) = load_state(storage, review, &blocks)?;
    review.decision_version = decision_version(review, &state)?;
    review.current = contents(review, &blocks, &state, false)?;
    let fully_decided = blocks
        .iter()
        .all(|block| state.blocks.contains_key(&block.id))
        && (!structural(review, &blocks) || state.structural.is_some());
    review.resolved = review.accepted || fully_decided;
    review.accepted |= fully_decided
        && state
            .blocks
            .values()
            .all(|decision| *decision == FileReviewDecision::Accept)
        && state.structural != Some(FileReviewDecision::Reject);
    let mut old_shift = 0isize;
    let mut new_shift = 0isize;
    let mut pending = Vec::new();
    if !review.resolved {
        for block in &blocks {
            let removed = block.hunk.removed_lines.len() as isize;
            let added = block.hunk.new_rows.len() as isize;
            match state.blocks.get(&block.id) {
                Some(FileReviewDecision::Accept) => old_shift += added - removed,
                Some(FileReviewDecision::Reject) => new_shift += removed - added,
                None => {
                    let shifted = |row: usize, shift: isize| {
                        row.checked_add_signed(shift)
                            .ok_or_else(|| invalid("El rango revisado ya no es válido"))
                    };
                    let mut hunk = block.hunk.clone();
                    hunk.new_rows = shifted(hunk.new_rows.start, new_shift)?
                        ..shifted(hunk.new_rows.end, new_shift)?;
                    hunk.old_start_row = shifted(hunk.old_start_row, old_shift)?;
                    pending.push(FileReviewBlock {
                        id: block.id.clone(),
                        hunk,
                    });
                }
            }
        }
    }
    review.blocks = pending.into();
    if state != Decisions::default() {
        let base = contents(review, &blocks, &state, true)?;
        let (mut file, truncated, _) = snapshot_diff_until(
            storage,
            &review.path,
            base.as_deref().unwrap_or_default().as_bytes(),
            review.current.as_deref().unwrap_or_default().as_bytes(),
            MAX_DIFF_BYTES,
            Instant::now() + PROJECTION_BUDGET,
        )?;
        file.status = review.original_file.status.clone();
        file.old_path = review.old_path.clone();
        review.file = file;
        if truncated && review.acceptance_unavailable.is_none() {
            review.acceptance_unavailable = Some(
                "El diff pendiente está incompleto; no se puede decidir esta revisión parcial"
                    .into(),
            );
        }
    }
    if review.acceptance_unavailable.is_none() {
        review.acceptance_unavailable = disk_mismatch(
            &review.workspace,
            &review.path,
            review.current.as_deref().map(str::as_bytes),
        );
    }
    if review.acceptance_unavailable.is_none() {
        if let Some(source) = &review.old_path {
            review.acceptance_unavailable = disk_mismatch(&review.workspace, source, None);
        }
    }
    review.can_accept = !review.resolved && review.acceptance_unavailable.is_none();
    review.rejection_unavailable = review.acceptance_unavailable.clone().or_else(|| review.old_path.as_ref().map(|_| {
        "El renombrado afecta dos rutas. Reviértelo manualmente para conservar ambas de forma segura.".into()
    }));
    review.can_reject = !review.resolved && review.rejection_unavailable.is_none();
    Ok(())
}

/// Applies one versioned decision. Call on a worker, with a clean host buffer.
/// The workspace lock prevents cooperating tools/editor saves from interleaving.
pub(crate) fn decide_file_review(
    storage: &Path,
    session: &str,
    workspace: &Path,
    call_id: &str,
    path: &str,
    expected_version: &str,
    expected_decision_version: &str,
    target: FileReviewDecisionTarget,
    decision: FileReviewDecision,
) -> io::Result<FileReviewDecisionResult> {
    let guard = lock_workspace(storage, workspace)?;
    let review = load_guarded(storage, session, workspace, call_id, path)?;
    if review.version != expected_version || review.decision_version != expected_decision_version {
        return Err(invalid(
            "La revisión cambió. Abre de nuevo sus bloques antes de decidir.",
        ));
    }
    if review.resolved {
        return Ok(FileReviewDecisionResult {
            review,
            disk_changed: false,
            warning: None,
        });
    }
    let allowed = if decision == FileReviewDecision::Accept {
        review.can_accept
    } else {
        review.can_reject
    };
    if !allowed {
        return Err(invalid(if decision == FileReviewDecision::Accept {
            review
                .acceptance_unavailable
                .as_deref()
                .unwrap_or("Esta revisión no se puede aceptar")
        } else {
            review
                .rejection_unavailable
                .as_deref()
                .unwrap_or("Esta revisión no se puede rechazar")
        }));
    }
    let blocks = original_blocks(&review)?;
    let (mut state, step) = load_state(storage, &review, &blocks)?;
    if step + 1 >= MAX_STEPS {
        return Err(invalid("Esta revisión alcanzó su límite de decisiones"));
    }
    match target {
        FileReviewDecisionTarget::File => {
            for block in &blocks {
                state.blocks.entry(block.id.clone()).or_insert(decision);
            }
            if structural(&review, &blocks) {
                state.structural.get_or_insert(decision);
            }
        }
        FileReviewDecisionTarget::Block(id) => {
            if !review.blocks.iter().any(|block| block.id == id) {
                return Err(invalid("El bloque ya no está pendiente en esta revisión"));
            }
            state.blocks.insert(id, decision);
        }
    }
    validate_state(&review, &blocks, &state)?;
    let next = contents(&review, &blocks, &state, false)?;
    let disk_changed = next != review.current;
    let record = DecisionRecord {
        schema_version: 1,
        evidence_version: review.version.clone(),
        step: step + 1,
        previous: review.decision_version.clone(),
        state,
        disk_changed,
    };
    let sha256 = digest(&serde_json::to_vec(&record).map_err(io::Error::other)?);
    let encoded = serde_json::to_vec(&StoredDecision {
        sha256,
        payload: record,
    })
    .map_err(io::Error::other)?;
    if encoded.len() > MAX_RECORD_BYTES {
        return Err(invalid("Las decisiones superan su límite de metadatos"));
    }
    let directory = Directory::open_named(storage, DECISIONS, true)?
        .ok_or_else(|| invalid("No se pudo abrir el registro de decisiones"))?;
    if fs::read_dir(storage.join(DECISIONS))?
        .take(MAX_RECEIPTS)
        .count()
        >= MAX_RECEIPTS
    {
        return Err(invalid("El registro de decisiones alcanzó su límite"));
    }
    let staged = format!(".{}.tmp", uuid::Uuid::new_v4());
    let committed = name(&review, step + 1, false);
    let pending = name(&review, step + 1, true);
    let result = (|| {
        let mut file = directory.create(&staged)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        revalidate_acceptance(storage, &review)?;
        let warning = if disk_changed {
            directory.publish(&staged, &pending)?;
            directory.sync()?;
            let written = crate::code_editor_files::write_review_file_guarded(
                &guard,
                Path::new(&review.path),
                review.current.as_deref(),
                next.as_deref(),
                || revalidate_acceptance(storage, &review),
            );
            match written {
                Ok(warning) => {
                    directory.publish(&pending, &committed)?;
                    directory.sync()?;
                    directory.remove(&pending)?;
                    warning
                }
                Err(error) => {
                    if disk_mismatch(
                        &review.workspace,
                        &review.path,
                        review.current.as_deref().map(str::as_bytes),
                    )
                    .is_none()
                    {
                        directory.remove(&pending)?;
                        directory.sync()?;
                    }
                    return Err(io::Error::other(error));
                }
            }
        } else {
            directory.publish(&staged, &committed)?;
            None
        };
        directory.sync()?;
        let review = load_guarded(storage, session, workspace, call_id, path)?;
        Ok(FileReviewDecisionResult {
            review,
            disk_changed,
            warning,
        })
    })();
    let _ = directory.remove(&staged);
    result
}

pub(super) fn remove_session(storage: &Path, session: &str) -> io::Result<()> {
    let Some(directory) = Directory::open_named(storage, DECISIONS, false)? else {
        return Ok(());
    };
    let prefix = format!("{session}.");
    for entry in fs::read_dir(storage.join(DECISIONS))?.take(MAX_RECEIPTS + 1) {
        if let Some(name) = entry?
            .file_name()
            .to_str()
            .filter(|name| name.starts_with(&prefix) && name.ends_with(".json"))
        {
            directory.remove(name)?;
        }
    }
    directory.sync()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        storage: PathBuf,
        events: Vec<AgentEvent>,
    }

    impl Fixture {
        fn new(before: Option<&str>, after: Option<&str>) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("project");
            let storage = temp.path().join("data");
            fs::create_dir(&root).unwrap();
            fs::create_dir(&storage).unwrap();
            if let Some(before) = before {
                fs::write(root.join("file.txt"), before).unwrap();
            }
            let events = vec![AgentEvent::new(
                "decisions",
                1,
                AgentEventKind::SessionStarted {
                    workspace: root.clone(),
                },
            )];
            capture_baseline(&storage, "decisions", &root, &events).unwrap();
            let mut fixture = Self {
                _temp: temp,
                root,
                storage,
                events,
            };
            let call = if let Some(after) = after {
                fs::write(fixture.root.join("file.txt"), after).unwrap();
                ToolCall {
                    id: "edit".into(),
                    name: "write_file".into(),
                    arguments: json!({"path":"file.txt","content":after}),
                }
            } else {
                fs::remove_file(fixture.root.join("file.txt")).unwrap();
                ToolCall {
                    id: "edit".into(),
                    name: "apply_patch".into(),
                    arguments: json!({"patch":"*** Begin Patch\n*** Delete File: file.txt\n*** End Patch\n"}),
                }
            };
            fixture.event(AgentEventKind::ToolStarted { tool_call: call });
            fixture.event(AgentEventKind::ToolFinished {
                tool_call_id: "edit".into(),
                output: "ok".into(),
                is_error: false,
            });
            fixture
        }

        fn event(&mut self, event: AgentEventKind) {
            self.events.push(AgentEvent::new(
                "decisions",
                self.events.len() as u64 + 1,
                event,
            ));
            let mut bytes = Vec::new();
            for event in &self.events {
                serde_json::to_writer(&mut bytes, event).unwrap();
                bytes.push(b'\n');
            }
            fs::write(self.storage.join("decisions.jsonl"), bytes).unwrap();
        }

        fn load(&self) -> FileReview {
            load_file_review(&self.storage, "decisions", &self.root, "edit", "file.txt").unwrap()
        }

        fn decide(
            &self,
            review: &FileReview,
            target: FileReviewDecisionTarget,
            decision: FileReviewDecision,
        ) -> io::Result<FileReviewDecisionResult> {
            decide_file_review(
                &self.storage,
                "decisions",
                &self.root,
                "edit",
                "file.txt",
                &review.version,
                &review.decision_version,
                target,
                decision,
            )
        }

        fn stage_rejection(&self, review: &FileReview) {
            let blocks = original_blocks(review).unwrap();
            let state = Decisions {
                blocks: blocks
                    .iter()
                    .map(|block| (block.id.clone(), FileReviewDecision::Reject))
                    .collect(),
                structural: structural(review, &blocks).then_some(FileReviewDecision::Reject),
            };
            let payload = DecisionRecord {
                schema_version: 1,
                evidence_version: review.version.clone(),
                step: 1,
                previous: review.decision_version.clone(),
                state,
                disk_changed: true,
            };
            let sha256 = digest(&serde_json::to_vec(&payload).unwrap());
            let directory = Directory::open_named(&self.storage, DECISIONS, true)
                .unwrap()
                .unwrap();
            let mut file = directory.create(&name(review, 1, true)).unwrap();
            file.write_all(&serde_json::to_vec(&StoredDecision { sha256, payload }).unwrap())
                .unwrap();
            file.sync_all().unwrap();
            directory.sync().unwrap();
        }
    }

    #[test]
    fn mixed_block_decisions_keep_accepted_bytes_and_exact_unicode_crlf_eof() {
        let before = "αold\r\nkeep1\r\nβold\r\nkeep2\r\nγold";
        let after = "αnew\r\nkeep1\r\nβnew\r\nkeep2\r\nγnew";
        let fixture = Fixture::new(Some(before), Some(after));
        let original = fixture.load();
        assert_eq!(original.blocks.len(), 3);
        let journal = fs::read(fixture.storage.join("decisions.jsonl")).unwrap();
        let baseline = fs::read(baseline_path(&fixture.storage, "decisions").unwrap()).unwrap();
        let accepted = fixture
            .decide(
                &original,
                FileReviewDecisionTarget::Block(original.blocks[0].id.clone()),
                FileReviewDecision::Accept,
            )
            .unwrap();
        assert!(!accepted.disk_changed && !accepted.review.resolved);
        assert_eq!(accepted.review.current.as_deref(), Some(after));
        assert_eq!(accepted.review.blocks.len(), 2);
        assert_eq!(
            (
                accepted.review.file.additions,
                accepted.review.file.deletions
            ),
            (2, 2)
        );
        let third = accepted
            .review
            .blocks
            .iter()
            .find(|block| block.hunk.new_rows.start == 4)
            .unwrap()
            .id
            .clone();
        let rejected = fixture
            .decide(
                &accepted.review,
                FileReviewDecisionTarget::Block(third),
                FileReviewDecision::Reject,
            )
            .unwrap();
        assert!(rejected.disk_changed && !rejected.review.resolved);
        assert_eq!(
            fs::read_to_string(fixture.root.join("file.txt")).unwrap(),
            "αnew\r\nkeep1\r\nβnew\r\nkeep2\r\nγold"
        );
        assert_eq!(rejected.review.blocks.len(), 1);
        let finished = fixture
            .decide(
                &rejected.review,
                FileReviewDecisionTarget::File,
                FileReviewDecision::Reject,
            )
            .unwrap();
        assert!(finished.review.resolved && !finished.review.accepted);
        assert!(finished.review.blocks.is_empty());
        assert_eq!(
            finished.review.after.as_deref(),
            Some(after),
            "immutable tool AFTER never changes"
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("file.txt")).unwrap(),
            "αnew\r\nkeep1\r\nβold\r\nkeep2\r\nγold"
        );
        let reopened = fixture.load();
        assert!(reopened.resolved && !reopened.can_accept && !reopened.can_reject);
        assert_eq!(reopened.current, finished.review.current);
        assert_eq!(
            fs::read(fixture.storage.join("decisions.jsonl")).unwrap(),
            journal
        );
        assert_eq!(
            fs::read(baseline_path(&fixture.storage, "decisions").unwrap()).unwrap(),
            baseline
        );
    }

    #[test]
    fn rejection_shifts_remaining_rows_and_accepting_rest_does_not_restore_rejected_bytes() {
        let fixture = Fixture::new(
            Some("old1\nold2\ncontext\nold3\n"),
            Some("new1\ncontext\nnew3\n"),
        );
        let original = fixture.load();
        assert_eq!(original.blocks.len(), 2);
        let remaining_id = original.blocks[1].id.clone();
        let rejected = fixture
            .decide(
                &original,
                FileReviewDecisionTarget::Block(original.blocks[0].id.clone()),
                FileReviewDecision::Reject,
            )
            .unwrap();
        assert_eq!(rejected.review.blocks[0].id, remaining_id);
        assert_eq!(rejected.review.blocks[0].hunk.new_rows, 3..4);
        let accepted = fixture
            .decide(
                &rejected.review,
                FileReviewDecisionTarget::File,
                FileReviewDecision::Accept,
            )
            .unwrap();
        assert!(!accepted.disk_changed && accepted.review.resolved);
        assert_eq!(
            fs::read_to_string(fixture.root.join("file.txt")).unwrap(),
            "old1\nold2\ncontext\nnew3\n"
        );
        assert_eq!(
            accepted.review.file.additions + accepted.review.file.deletions,
            0
        );
    }

    #[test]
    fn stale_decisions_external_writes_and_new_admissions_never_overwrite_current_bytes() {
        let mut fixture =
            Fixture::new(Some("old1\ncontext\nold2\n"), Some("new1\ncontext\nnew2\n"));
        let original = fixture.load();
        let accepted = fixture
            .decide(
                &original,
                FileReviewDecisionTarget::Block(original.blocks[0].id.clone()),
                FileReviewDecision::Accept,
            )
            .unwrap();
        assert!(
            fixture
                .decide(
                    &original,
                    FileReviewDecisionTarget::File,
                    FileReviewDecision::Reject
                )
                .is_err()
        );
        fs::write(fixture.root.join("file.txt"), "my manual changes\n").unwrap();
        assert!(
            fixture
                .decide(
                    &accepted.review,
                    FileReviewDecisionTarget::File,
                    FileReviewDecision::Reject
                )
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("file.txt")).unwrap(),
            "my manual changes\n"
        );
        fs::write(
            fixture.root.join("file.txt"),
            accepted.review.current.as_deref().unwrap(),
        )
        .unwrap();
        fixture.event(AgentEventKind::ToolStarted {
            tool_call: ToolCall {
                id: "later".into(),
                name: "write_file".into(),
                arguments: json!({"path":"file.txt","content":"later"}),
            },
        });
        assert!(
            fixture
                .decide(
                    &accepted.review,
                    FileReviewDecisionTarget::File,
                    FileReviewDecision::Reject
                )
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("file.txt")).unwrap(),
            "new1\ncontext\nnew2\n"
        );
    }

    #[test]
    fn rejecting_new_empty_file_removes_it_and_rejecting_deletion_restores_exact_content() {
        let empty = Fixture::new(None, Some(""));
        let review = empty.load();
        assert!(review.blocks.is_empty() && review.can_reject && !review.resolved);
        let result = empty
            .decide(
                &review,
                FileReviewDecisionTarget::File,
                FileReviewDecision::Reject,
            )
            .unwrap();
        assert!(result.disk_changed && result.review.resolved && result.review.current.is_none());
        assert!(!empty.root.join("file.txt").exists());

        let removed = Fixture::new(Some("restaurar 🦀\r\nsin salto final"), None);
        let review = removed.load();
        assert!(review.current.is_none() && review.can_reject);
        let result = removed
            .decide(
                &review,
                FileReviewDecisionTarget::File,
                FileReviewDecision::Reject,
            )
            .unwrap();
        assert!(result.disk_changed && result.review.resolved);
        assert_eq!(
            fs::read_to_string(removed.root.join("file.txt")).unwrap(),
            "restaurar 🦀\r\nsin salto final"
        );
    }

    #[test]
    fn interrupted_rejection_recovers_metadata_only_and_never_replays_over_manual_edits() {
        let committed = Fixture::new(Some("before\n"), Some("after\n"));
        let review = committed.load();
        committed.stage_rejection(&review);
        fs::write(committed.root.join("file.txt"), "before\n").unwrap();
        assert!(committed.load().resolved);
        assert!(
            !committed
                .storage
                .join(DECISIONS)
                .join(name(&review, 1, true))
                .exists()
        );
        assert!(
            committed
                .storage
                .join(DECISIONS)
                .join(name(&review, 1, false))
                .exists()
        );
        assert_eq!(
            fs::read_to_string(committed.root.join("file.txt")).unwrap(),
            "before\n"
        );

        let uncommitted = Fixture::new(Some("before\n"), Some("after\n"));
        let review = uncommitted.load();
        uncommitted.stage_rejection(&review);
        assert!(!uncommitted.load().resolved);
        assert_eq!(
            fs::read_to_string(uncommitted.root.join("file.txt")).unwrap(),
            "after\n"
        );

        let conflict = Fixture::new(Some("before\n"), Some("after\n"));
        let review = conflict.load();
        conflict.stage_rejection(&review);
        fs::write(conflict.root.join("file.txt"), "my independent work\n").unwrap();
        assert!(
            load_file_review(
                &conflict.storage,
                "decisions",
                &conflict.root,
                "edit",
                "file.txt"
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(conflict.root.join("file.txt")).unwrap(),
            "my independent work\n"
        );
        assert!(
            conflict
                .storage
                .join(DECISIONS)
                .join(name(&review, 1, true))
                .exists()
        );
    }

    #[test]
    fn rename_keeps_structural_acceptance_separate_and_never_offers_an_unsafe_two_path_rejection() {
        let mut fixture = Fixture::new(Some("old\n"), Some("new\n"));
        fs::rename(
            fixture.root.join("file.txt"),
            fixture.root.join("moved.txt"),
        )
        .unwrap();
        fixture.events[1].kind = AgentEventKind::ToolStarted {
            tool_call: ToolCall {
                id: "edit".into(),
                name: "apply_patch".into(),
                arguments: json!({"patch":"*** Begin Patch\n*** Update File: file.txt\n*** Move to: moved.txt\n@@\n-old\n+new\n*** End Patch\n"}),
            },
        };
        let mut journal = Vec::new();
        for event in &fixture.events {
            serde_json::to_writer(&mut journal, event).unwrap();
            journal.push(b'\n');
        }
        fs::write(fixture.storage.join("decisions.jsonl"), journal).unwrap();
        let review = load_file_review(
            &fixture.storage,
            "decisions",
            &fixture.root,
            "edit",
            "moved.txt",
        )
        .unwrap();
        assert!(review.can_accept && !review.can_reject);
        assert!(
            review
                .rejection_unavailable
                .as_deref()
                .unwrap()
                .contains("dos rutas")
        );
        assert!(
            decide_file_review(
                &fixture.storage,
                "decisions",
                &fixture.root,
                "edit",
                "moved.txt",
                &review.version,
                &review.decision_version,
                FileReviewDecisionTarget::File,
                FileReviewDecision::Reject
            )
            .is_err()
        );
        let partial = decide_file_review(
            &fixture.storage,
            "decisions",
            &fixture.root,
            "edit",
            "moved.txt",
            &review.version,
            &review.decision_version,
            FileReviewDecisionTarget::Block(review.blocks[0].id.clone()),
            FileReviewDecision::Accept,
        )
        .unwrap();
        assert!(
            partial.review.blocks.is_empty()
                && !partial.review.resolved
                && partial.review.can_accept
        );
        let done = decide_file_review(
            &fixture.storage,
            "decisions",
            &fixture.root,
            "edit",
            "moved.txt",
            &partial.review.version,
            &partial.review.decision_version,
            FileReviewDecisionTarget::File,
            FileReviewDecision::Accept,
        )
        .unwrap();
        assert!(done.review.resolved && done.review.accepted && !done.disk_changed);
        assert!(!fixture.root.join("file.txt").exists());
        assert_eq!(
            fs::read_to_string(fixture.root.join("moved.txt")).unwrap(),
            "new\n"
        );
    }
}
