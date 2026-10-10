# Editor saves and chat change history

The native editor saves real files. A manual save must not appear as an AI tool
or make a later AI edit use an invented “before” snapshot.

## Save protocol

Production calls `EditorFileWorker::try_save_with_history(scope, snapshot,
expected_digest, storage_root)`. Queue admission is nonblocking. The worker
validates the canonical workspace and relative file path, acquires the shared
workspace guard, checks the current digest and builds a private temporary file.

While holding the guard, it enumerates **all** stored chat baselines and validates
their checksum, content limits and workspace scope. For each baseline of the
same workspace it validates the bounded journal and durably publishes a private
`session-changes-editor/<session_id>.json` marker. Each marker stores the first
`after_sequence` boundary and is independent of the external MCP marker.

Only after all markers are durable does the worker recheck the file identity,
workspace binding and digest and atomically replace the document. Parent-directory
sync failures after a successful replacement remain explicit durability warnings.
No `ToolStarted` or `ToolFinished` event is synthesized or appended.

A failed marker, malformed journal, changed digest, missing history store or
exceeded history budget cancels the save. The original document remains intact
and the temporary file is removed. Some markers may have been published before
a later marker failed: those markers conservatively limit subsequent replay even
though the file was not replaced. Retrying is safe; the first valid boundary is
retained. A prior boundary is never advanced to hide an earlier manual save.

## Concurrent capture and pending effects

`capture_baseline` takes the same workspace guard through snapshot capture and
publication. A chat baseline is therefore captured completely before or after a
cooperating editor save; it cannot observe its intermediate temporary state.
A baseline captured after a save starts from the new file contents and needs no
marker for that earlier save.

The guard is an advisory file lock named by a SHA-256 hash of the canonical
workspace, under the private history directory. It applies across threads and
processes sharing the same session store. Lock files remain present permanently
so deleting a lock inode cannot split the lock protocol. Locking is worker-only;
UI rendering and event admission never wait on it. The implementation uses
[Rust `File::try_lock`](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).

Tool execution and external editors do not participate in this lock. If a write,
patch, opaque actor or hook is still pending when a journal is read, the marker
cut moves conservatively before its start. Journal append races can therefore
invalidate extra later effects but cannot assert an unverified ordering. A partial
JSONL tail fails with a retryable error. No process is restarted, no journal is
repaired and no external writer is suppressed.

## Replay and visual meaning

The existing shared replay engine applies the editor barrier only to events
**after** its recorded sequence. Earlier confirmed per-tool diffs remain valid,
including cached historical results. A later tool has no verified before-image
across the manual boundary. Its inline diff reports data unavailable rather than
using the current worktree or attributing manual text to the AI. A confirmed full
`write_file` still provides a known after-image; patches cannot assume their input
across an unknown epoch.

A cumulative baseline-to-confirmed-full-write comparison can remain computable.
It carries an explicit coverage note: manual editor saves occurred and the net
comparison does not represent changes exclusively made by the AI. It is a
journal projection, not proof of the latest on-disk file contents. Files changed
by external processes remain governed by the existing opaque-effect policies.

Deleting a chat removes its editor marker only after its baseline is removed and
the host has committed the chat deletion. Editor saves never alter a baseline.

## Deciding a reviewed agent edit

Accepting and saving are separate operations. The editor review reconstructs the
complete recorded before/after versions for one confirmed tool, path and chat.
It compares the active saved document with the expected reviewed image before
offering decisions; a read-only deleted-file snapshot can instead acknowledge the
confirmed deletion. Proposals, incomplete effects, stale versions and unknown
before-images do not gain actionable review controls.

The breadcrumb's **Aceptar cambios** accepts the remaining file changes;
**Rechazar cambios** restores the pending changes for the whole file. The
active block exposes **Aceptar** and **Rechazar**. Accepting only changes private
review metadata: it folds the chosen block's current lines into the comparison
base and removes that pending block without writing project bytes. Rejecting
restores that block's exact recorded old bytes, retaining other accepted and
pending edits. The host reloads the clean buffer after a disk-changing decision;
it never silently replaces a dirty draft.

`session_file_review::decide_file_review` runs off the UI thread and validates both
the immutable evidence version and the current decision version under the workspace
history guard. Stable block identities survive changes in pending row offsets.
Decisions are stored in a checksummed private chain under
`session-file-review-decisions`; older whole-file receipts remain readable. For a
rejection, the backend records an intent before guarded file replacement. Recovery
only confirms already-observed old or new bytes; it does not retry a file write
while browsing history. Unexpected external content leaves the decision unresolved.

The decision store does not change the Git index, chat baseline, original journal
or recorded historical diff. A newer agent effect is a separate review version.
The host rejects actions from an old tab or revision and disables them while the
relevant execution is in progress, the editor is dirty, or content conflicts with
the expected disk image. Rejecting a rename is unavailable because safely restoring
it requires resolving both paths. Accepting is not an editor undo operation.

## Limits and filesystem safety

History preparation limits are 2,048 baseline sessions, 8,192 directory entries,
128 MiB total baseline and matching-journal read sizes, and an eight-second
cooperative budget checked between bounded filesystem/parse operations. Existing
per-baseline, file, event and journal limits still apply. Exceeding a limit fails
the save; no matching chat is silently omitted. A separate eight-second lock-wait
budget reports contention and allows retry. Local filesystem operations may
still block inside the operating system; these budgets do not interrupt a
filesystem call.

Unix readers walk the storage ancestry with `openat` and `O_NOFOLLOW`, retain
anchored directory handles, reject symlink final files and require owner-only
history permissions (directory 0700, marker/lock 0600). The guard checks storage
and marker-directory identities before and after marker publication. Document
writes retain the existing directory-handle, digest and atomic-rename checks.

The portable fallback rejects symlinks/reparse points and inherits the profile
ACL. Its path checks cannot provide the same parent-rename race protection as
Unix anchored descriptors. Neither platform offers a filesystem compare-and-swap
for an uncooperative writer in the final digest-check-to-rename interval; a
separate process can still race that interval.

## Regression coverage

Offline tests exercise multiple matching chats and a foreign workspace,
independent MCP markers, untouched journal bytes, retained earlier inline diffs,
unknown later before-images, pending edit/hook cuts, malformed evidence and
retry, limits and deadlines, private permissions, symlink rejection, lock
contention, independent workspaces and concurrent baseline capture. File-worker
and host tests exercise the real Save action and verify a failed marker does not
replace a document. No test performs model inference or elevates privileges.
