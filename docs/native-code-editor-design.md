# Native code editor: buffer, display and file authority

## Implemented core

`crates/openide-editor` is an independent Rust editor core. It does not import
Zed, GPUI or a GUI widget. `openide-ui` remains the component framework; its
renderer consumes a cheap immutable `BufferSnapshot`, and input acts on the
host-owned `TextBuffer` through a short controller lock.

The core uses Ropey 1.6.1, unicode-segmentation 1.13.3, unicode-width 0.2.2
and the regex crate for bounded searches. Ropey's shared clones permit snapshots without copying the whole file.
Its character and line indexing also avoids a linear scan of preceding lines
when the viewport moves. See the [Ropey API](https://docs.rs/ropey/1.6.1/ropey/struct.Rope.html).
Grapheme lookup uses the chunk-aware
[GraphemeCursor](https://docs.rs/unicode-segmentation/1.13.3/unicode_segmentation/struct.GraphemeCursor.html)
instead of flattening the rope. Display columns use
[unicode-width](https://docs.rs/unicode-width/0.2.2/unicode_width/) plus explicit tab stops.

### Coordinate contract

- `DocumentId` is a process-local stable `u64`. Host paths and workspace identity
  remain outside the buffer and must not be inferred from the ID.
- All `Selection`, `TextEdit` and range offsets count Unicode scalar values
  (`char`), not bytes, UTF-16 units or pixels. Selections retain their direction.
- `Position` contains a zero-based physical row and scalar column. Lines have
  their original terminators; `line_range` and `line_text` exclude those terminators.
- `offset_of` validates positions; `clipped_offset_of` clips and snaps a caret to
  a grapheme boundary. Offsets inside CRLF map to the preceding line's final
  content column. Interactive clients use grapheme boundaries for caret placement.
- Exact programmatic `edit` ranges may split a grapheme. Interactive Backspace
  and Delete expand even a partial programmatic selection to whole extended
  graphemes, including emoji sequences, combining marks and CRLF.
- Tab columns and physical columns are distinct. `display_column` and
  `offset_for_display_column` map them, including wide glyphs and combined emoji.

### Edits, history and dirty state

`edit` validates its original range, result selection, byte budget and revision
before touching the document. `apply_transaction` validates every original
range and rejects overlaps or duplicate insertion offsets, then applies all
edits in descending source order. One transaction produces one revision and
one undo entry.

`validate_transaction` exposes the same range, selection, size and revision
checks without mutation. The host uses it to preflight all buffers of a language
workspace edit while holding their locks.

History stores shared before/after rope states, the directed primary/secondary
selection set and line-ending preference.
Undo and redo restore the complete state without replaying text into a String.
Adjacent typing or deletion can coalesce within 500 ms. Movement, saving,
paste, newline and an explicit `break_undo_group` form boundaries. The maximum
group length also forms a boundary; backwards timestamps never merge groups.

The public revision always increases for an edit, undo or redo. A separate
content-state identity records which historical state was saved. Consequently,
undoing to saved content becomes clean without rolling the revision backwards.
`mark_saved_snapshot` acknowledges the actual snapshot written by an asynchronous
worker; edits made while that worker writes stay dirty. A foreign document's
snapshot is rejected. No-op text edits leave the revision and redo intact.

Existing UTF-8 bytes, LF, CRLF, CR and mixed line endings remain exact. Pasted
line endings are not normalized. `insert_newline` uses the first LF/CRLF/CR
convention in the initial document and copies the current leading spaces/tabs
up to the caret. A document without those terminators defaults to LF. Explicit
`SetLineEnding` conversion is an undoable transaction; ordinary paste preserves
its source terminators.

Word commands use rope character iterators without flattening the document.
Unicode alphanumeric characters, underscores and combining marks form identifier
runs; punctuation is a separate stop. Horizontal whitespace can be crossed,
while line breaks are separate stops. Targets and deletions remain on extended
grapheme boundaries. `previous_word_start`, `next_word_end`,
`delete_word_backward` and `delete_word_forward` support Ctrl+Arrow and
Ctrl+Backspace/Delete. Smart Home toggles between the first non-indent character
and physical column zero; Ctrl+Home/End still targets document boundaries.

`indent_selection` inserts spaces to the next visual tab stop for an empty
selection. Tab/Shift+Tab on selected lines creates one atomic transaction of
prefix edits, preserving directed selection, original line terminators and undo.
An endpoint at the next line's column zero excludes that line. Outdent removes
one leading tab or spaces to the previous indentation stop, clipping carets
inside removed indentation. More than 4096 selected lines is explicitly rejected
before mutation, following the existing transaction limit.

### Commands, selections and search

`EditorCommand` is the common entry point for toolbar actions, shortcuts and
context menus. The core retains a primary selection plus up to 255 additional
selections. It normalizes overlaps and duplicate carets, preserves the primary
identity and performs multi-cursor typing/deletion/indentation as one transaction.
Occurrence selection, vertical cursor addition and rectangular selection use the
same source-coordinate model. Undo/redo restore the entire selection set.

Commands also cover duplicate/move/delete lines, line comments, goto, tab/space
indent styles, delimiter pairing and skipping an existing closing delimiter.
Auto-pairing and auto-indent are configurable editing heuristics, not a full
language formatter. Snippets support numbered/linked tabstops, defaults and a
final stop; Tab/Shift+Tab navigates the active session and Escape cancels it.
This is an explicit supported snippet subset, not the entire TextMate grammar.

`SearchOptions` supports case sensitivity, whole words, regular expressions and
a captured selection scope. Searches return document/revision-tagged matches;
the host debounces and runs search off the frame thread. Results are limited to
4096 matches, with a visible truncated state. Regex compilation has bounded
2 MiB program and DFA budgets. Replacing one or all matches validates the same
snapshot and commits one transaction; a truncated Replace All is rejected rather
than silently replacing a subset.

### Explicit bounds

Defaults allow an 8 MiB UTF-8 document, 512 history transactions, 64 coalesced
edits per group and a 32 MiB history accounting budget. The history budget
charges inserted/removed bytes plus 2 KiB per edit for shared storage overhead;
it is an accounting limit, **not a measured allocator RSS ceiling**. Entry count
also bounds overhead. The host can supply `EditorLimits`; zero history budget
disables undo while preserving editing. Transactions contain at most 4096 edits.
Failures are typed `EditorError` values and rejected edits leave content intact.

Snapshots expose borrowed `line`, `slice` and `chunks`, plus `write_to`. Export
requires no full-file String and may run off the UI thread. `line_text` and
`text_range` are explicit allocating conveniences; a renderer should use
`display_line_window` for long lines.

`visible_lines` returns only the requested rows with overscan (maximum 1024).
`display_line_window` expands a horizontal slice (maximum 8192 columns and
64 KiB output). Tabs are expanded only for painting. A pathological grapheme
larger than 64 KiB is visually clipped and marked `truncated`; its underlying
source remains complete. Width for that exceptional cluster uses the maximum
scalar width to avoid allocating the entire cluster. Horizontal seeking still
walks graphemes within the requested line. Width-keyed display caches avoid
repeating unchanged visible work; they do not constitute a full shaping engine
or a precomputed pixel index of every source character.

## Implemented framework components

`CodeEditor`, `CodeDiff` and `CodeLanguage` are reusable `openide-ui` components.
They use the existing `Element`/`UiTree` layout and `DrawCommand` renderer. Their
default viewport is 320 px high, with 13 px monospace text and 20 px physical
rows; `.height(...)` or the host's flex allocation controls the visible area.
The host supplies the exact native font row height via
`UiTree::set_text_height_measurer`. Code, syntax runs, gutter numbers and diff
markers share one vertically centered text origin inside each 20 px row. The
caret, selection rectangles, click mapping and scrolling retain that row grid.
The status bar centers the same font box independently in its 28 px footer.
Hosts without that callback use their calibrated monospace `FontMetrics`.
The view paints only visible rows and horizontal text windows, including line
numbers, selection, caret and draggable horizontal/vertical scrollbars. It does
not generate one element or one full-file String per source line.

`EditorController` shares the host-owned `Arc<Mutex<TextBuffer>>`. Input briefly
locks the buffer to acquire a snapshot and apply a checked edit; text measurement,
painting and file I/O happen outside that lock. Pointer, Shift-click/drag,
grapheme/word navigation, multiple carets, rectangular selection, smart Home/End,
PageUp/PageDown, Tab/Shift+Tab, Enter, copy/cut/paste and undo/redo operate on the
core's source coordinates. IME preedit is temporary display state tied to its
owner; committing inserts one transaction, while focus/document changes reject
a stale composition. Context menus preserve the existing selection and use
explicit clipboard actions. Clipboard requests are
limited to 1,048,576 selected scalar values. The editable component emits
`EditorChanged` and `EditorSaveRequested` with the view ID, document ID and
revision; it never emits the complete document as `InputChanged`.

Viewport state belongs to a visual ID. Rebuilding a component with the same
ID and document preserves scroll; replacing its document resets that viewport.
Discovered horizontal extents reset when the revision, tab width, font metrics
or viewport width changes. Replacing a long scrolled line with short text cannot
leave the new text hidden behind an obsolete horizontal offset.
`UiTree::forget_code_view` releases it on close. An LRU pass also evicts invisible
editor/diff views above the 16/64 retained-view targets; visible views are protected,
and viewport eviction never discards the host buffer. `reveal_code_caret` lets
the host request cursor visibility. Additive `Paste`, `PointerDownModified`
and `Scroll2D` events retain the existing TextArea and non-editor wheel behavior.
The focused unselected caret uses the same retained 500 ms blink clock as input
and textarea controls. Typing, navigation and actual focus changes restart its
visible phase; rebuilding an unchanged tree does not. The host schedules only
the next blink edge while idle. Window focus loss, an open context menu, a
selection or an offscreen caret removes the blink deadline. Reduced motion keeps
the caret solid; selection and active-line highlighting do not blink.

The optional `.status_bar(true)` paints current line/column, selected scalar
count, language and document metadata from the same snapshot as the code. It is
enabled in the application editor and remains current during pointer/keyboard
navigation without rebuilding the host root or sending synthetic edit events.
Narrow panels prioritize the cursor/selection position over secondary metadata;
the right-aligned gutter also distinguishes the active line.

### Visual rows, wrapping and folding

`VisualMap` maps physical source rows into wrapped/folded visual rows. Wrapping
uses display cells and keeps graphemes intact; a fold `[start,end)` retains its
first physical row. Painting, selection rectangles, hit testing, scrolling and
caret revelation share this mapping. A hidden caret expands the containing fold.
The map is cached by document/revision, width, tab width and fold state. Unfolded
edits rewrap changed physical lines while reusing unaffected prefix/suffix rows.
Initial maps and configuration changes still require source-dependent work; this
is not a guarantee of constant-time reflow for arbitrary giant documents.

Multiline selection uses one continuous native contour for each connected group
of visual rows, with rounded external and internal corners up to 3 px. Adjacent
rows share flat joins, so their translucent fill does not stack into darker
seams. Neighboring offscreen rows participate in contour construction to avoid
invented rounded ends at a viewport boundary. This changes only decoration:
source selection offsets, glyph advances, line height, hit testing, copy and
editing remain unchanged. Multiple selections retain independent contours.

### Indent guides, fold controls and sticky scope headers

Indent guides use visual tab columns on the existing 20 px row grid. They paint
only in the visible code body and move with horizontal scrolling. Wrapped
continuations do not acquire invented indentation. Blank rows bridge nearby
indentation using a bounded search; Python's indentation semantics use a separate
fallback rule. The active guide is brighter while the editor is focused. The
cache is keyed by document/revision/tab width, retains at most 4096 rows, examines
at most 256 neighboring rows in either direction and caps indentation at 512
columns. Guides are presentation heuristics; they do not modify the source or
replace the parser's scope ranges.

The host converts real Tree-sitter fold ranges and symbols into
`CodeFoldCandidate` and `CodeStickyScope` values. `.structure(document, revision,
folds, scopes)` accepts immutable arrays for one exact snapshot. The gutter shows
a chevron at a fold's first physical row, separate from line numbers and the
breakpoint lane. Clicking it changes the visual fold state without changing the
text, dirty state or revision. Selections that would end inside the hidden body
move to the retained header; other selections stay intact.

When declarations scroll above the viewport, the editor pins at most three
enclosing scope headers, using no more than 25% of the available code-body height.
Each displays the actual declaration source and takes
the caret back to that declaration when clicked. Header selection uses an
interval index over cached scope ranges; paint does not parse the document or
query a language server. Source rows, scroll coordinates and control hit regions
share the same visual map and viewport clip. Host action IDs contain the selected
tab, document and revision; delayed actions and parser results from another
snapshot are rejected. Stale structure disappears until current analysis arrives.

### Local breakpoint markers and DAP sessions

F9, the editor menu and the dedicated gutter lane toggle a local breakpoint.
Without a debug session its outlined circle remains pending; the tooltip does
not claim that it stops execution. A configured DAP session sends the complete
breakpoint set per source and obtains verification, rejection messages and
resolved positions from the real adapter. Verified marks use a filled circle,
rejected marks show a warning and disabled marks use a muted outline. A stopped
frame has a separate amber execution line and arrow; selecting another frame
changes the inspected location without editing the source.

Markers are scoped to workspace, path, document and revision, with up to 256
markers per file and 64 retained file entries. Any text edit, reload or undo that
changes the revision clears that file's markers and synchronizes the resulting
set with a connected adapter. This conservative policy avoids silently moving
them onto different source lines. Markers are not persisted across restarts.

The host reads explicit launch configurations from `.openide/debug.json`.
Loading a project, opening the debugger or reading that file does not execute
the program. Start launches the selected adapter executable and its argument
array through `openide-debug`, without a shell. The debugger panel occupies a
bounded section below the editor in the same right dock. Native controls expose
configuration selection, Start/Continue, Pause, Step Over, Step Into, Step Out
and Stop, plus real threads, frames, expandable variables and output. F5, F10,
F11 and Shift+F11 route through the same host actions; Shift+F10 remains the text
context menu.

`openide-debug` owns framed stdio DAP requests, responses, events and process
lifecycle. The host checks workspace/session and pause generations before
presenting inspection results; continuing removes obsolete frames and variable
references. The current configuration supports explicit `launch`, not attach,
conditional/hit-count breakpoints, persisted markers, debug REPL evaluation or
arbitrary reverse requests. Those capabilities must not be inferred from an
adapter supporting them. [Debugger validation](native-debugger-validation.md)
contains the reproducible configuration, real Python/Rust adapter probes and
native desktop evidence.

### Versioned Tree-sitter analysis and diff highlighting

`CodeLanguage` aliases `openide_language::Language`. `.language(...)` selects
plain text or actual Tree-sitter grammars for Rust, JavaScript, TypeScript, TSX,
Python, JSON, TOML, CSS and HTML. TypeScript and TSX remain distinct grammars.
`ParserEngine` updates cached trees for new snapshots and returns highlights,
symbols, folds, syntax-error state and an explicit truncated state. The parser
owns grammar/tree state; the renderer consumes only immutable analysis results.

`SyntaxCache` starts two shared workers lazily instead of one thread per view.
Its queue holds eight jobs; results require the exact document ID, revision and
language. Superseded jobs are cancelled, including during Tree-sitter parsing
and query execution. The display cache retains at most 100,000 rows and 262,144
spans, and skips giant lines above 256 KiB. Source text remains editable and
complete when presentation budgets are reached. Plain text does not start this
pool. `AnalysisWorker` provides a separate two-worker, last-wins service for the
host's outline, folds and external highlights. Parsing is never performed inside
a frame callback.

External `.highlights(document_id, revision, ranges)` are now used by the host
for parser results, rather than being a future integration point. Host analysis
spans are converted to native colors once on receipt and retained as an Arc.
`.diagnostics(document_id, revision, ranges)` adds gutter marks and underlines
only when a diagnostic belongs to the displayed snapshot.

`CodeDiff` consumes immutable `Arc<[CodeDiffLine]>` records with old/new line
numbers and exact source text. Diff metadata and presentation results are cached
by that identity. Isolated hunks use a bounded lexer with independent old/new
states; headers reset both because omitted predecessor source is unknown. This
is best-effort fragment highlighting, not an invented syntax tree for a missing
file. The component remains read-only and cannot apply or restore a patch.

### Language servers

`openide-language::lsp::LanguageServer` implements actual JSON-RPC/LSP over a
configured process's stdin/stdout. The client negotiates initialize/capabilities,
opens/changes/closes documents, cancels superseded requests, and supports
completion, hover, definitions, references, rename, formatting, code actions and
symbols. Requests and results carry URI, document ID, buffer revision and LSP
version. UTF-16 adapters use protocol CR/LF line semantics, reject surrogate
splits and maintain a bounded index cache. A reopened path receives a new
monotonic LSP version, so delayed diagnostics cannot attach to its new buffer.

The host exposes a server executable field in the editor options and a Connect
button. Configuration is scoped to project/language during the current run;
`OPENIDE_RUST_ANALYZER` and other language-specific overrides also participate in
server discovery. Candidates from PATH are not advertised as available until a
real protocol handshake succeeds. There is no automatic server installation.
Startup, full-document serialization and process retirement run off normal UI
frames. A reader wake callback schedules frames for incoming results; an idle
connection does not continuously repaint the application.

Completion edits adjust the resulting caret for additional edits before the
primary replacement. A completion without `textEdit` replaces only the contiguous
identifier prefix, never preceding whitespace or unrelated code. Rename,
formatting and code actions returning workspace edits are previewed. Targets load
without selecting their tabs; origin, versions, paths and every buffer's limits
are checked before applying all changes under locks. Changes stay unsaved and
undoable. File creation/rename/delete operations and command-only code actions
require separate host operations and are not silently executed. Only versioned
diagnostics draw native underlines; unversioned diagnostics remain visibly
unconfirmed in Problems.

Transport, parse budgets, API details and process fixtures are documented in
[Language services](language-services.md). The services do not supply every Zed
feature: there is no CRDT/collaborative buffer, full shaping/bidi engine, embedded
language-injection registry or general language-server command execution layer.

## Host integration and source of truth

Before this editor, `apps/agents-window/src/files.rs` offered a paged read-only
preview, not a document model. Its preview limits and per-page highlighting
must not become the editable text source: saving a clipped preview could destroy
the rest of a file. Editable buffers load the complete supported UTF-8 file or
return an explicit error. Opening a project text file now mounts `CodeEditor`
inside its right-dock file tab and loads the complete supported file, rather
than passing the paged preview into the editor. The host allows up to 16 MiB
of valid UTF-8 per editable file, overriding the core's 8 MiB default. Binary,
invalid UTF-8, oversized, non-regular and symlink paths produce explicit errors.

The file worker owns workspace/path validation, full-file loading, SHA-256 disk
identity, conflict detection and atomic replacement. Request and response queues
each hold at most eight operations. Loads may be superseded; admitted saves
retain their original snapshot and are not cancelled by switching or closing a
tab. Responses include file scope/generation, document ID and snapshot revision.
The host checks that scope and identity before updating a retained document.
`mark_saved_snapshot` acknowledges precisely the snapshot written: edits made
while saving stay dirty. A disk change observed before replacement is reported
as a conflict, preserving the editor's unsaved source.

The host retains at most 16 documents, preserving dirty buffers when a tab is
closed and reopened during the same application run. It can evict a clean,
inactive document; dirty or saving documents are not silently evicted. The
buffer does not execute tools, authenticate, access paths or write the journal.
File-save provenance and the narrow external-writer race around atomic rename
are documented in [Editor saves and history](editor-save-history.md).

Opening an existing file from chat activity also checks a retained clean buffer
against disk on the file worker. An unchanged digest preserves its controller,
selection and viewport. Changed disk content replaces only a still-clean buffer;
edits made while the check runs are retained, with a notice if disk also changed.
Missing files with recorded content open a separate immutable snapshot tab; they
are never created or saved merely by opening an unconfirmed tool proposal. See
[recorded file views](right-dock.md) for their scope and capture limits.

`session_changes.rs` already reconstructs per-chat before/after changes from a
captured baseline and **confirmed journal tool results**. That remains the
authority for chat diffs. Whole-working-tree Git changes belong to the separate
Git review surface. `ReviewFile.lines` already carries actual old/new line
numbers and row kinds for a virtualized `CodeDiff`. The per-tool chat diff is
loaded lazily when opened and remains read-only; limited, truncated and
unavailable coverage stays explicit. It does not replace missing baseline bytes
with the current disk file. A manual editor save has separate provenance and
does not fabricate a confirmed AI tool result. Opening a full
before/after document from a chat diff uses the explicit `FileReview` API;
current disk contents cannot stand in for a missing baseline.

### Reviewing confirmed agent edits in the document

The ordinary editable `CodeEditor` can show an exact review without replacing its
controller with a diff viewer. `CodeEditor::review(document_id, revision, hunks)`
accepts snapshot-bound `CodeReviewHunk` values: current source rows receive a
subtle green background and old source lines occupy red virtual rows before the
changed range. Virtual deletions are presentation data; they never enter the
buffer, saved file, source selection or clipboard. Empty modified ranges anchor
pure deletions, including deletions at the end of the document. Stale review
metadata disappears when the document or revision no longer matches.

The breadcrumb shares its row with compact change/file navigation,
separators, **Deshacer** and **Conservar** in a single row at every supported
width, including 240 px. Each arrow is a native 24 px icon button; decisions
remain text Buttons, with outlined undo and primary keep styling. Available
width is shared with the filename before choosing which navigation controls fit.
File navigation moves into overflow first, followed by change navigation.
Agents combines this overflow with editor actions and moves search and Markdown
preview there on narrow panes. IDE retains its separate tab-strip actions.
Both text decisions remain visible, and the filename keeps its complete path
in a tooltip when its visible label must fade. No extra review row is inserted.
Choosing an ordinary editor action closes the shared overflow and retains the
destination's normal focus, such as the search field.
Below 320 px, this combined menu moves shortcut and status hints to tooltips so
short action labels remain legible; keyboard bindings and disabled states do not
change. Other menus retain their usual inline hints.
There is no file stepper for a single file. Navigation stops at the first and last
item. File navigation uses the confirmed changes of the current conversation
and workspace, with no extra Git lookup. The controls stay outside the code fade.

The active changed block has its own **Deshacer** and **Conservar** text Buttons
inside the editor. `CodeEditor::review_actions` receives opaque action IDs in the
same order as the exact review hunks, with optional distinct IDs for its compact
change stepper when space allows. The widget is clipped to the code viewport;
pressing it emits a host request without changing the source selection or caret.
The host checks workspace, chat, path, tab, document revision, immutable evidence
version and decision version before acting. Pending work, unsaved edits, conflicts
and source bytes that differ from the expected reviewed document disable decisions.
A confirmed deleted file retains its immutable recorded diff and file decision actions without being recreated merely by opening
the review. Rejection explicitly restores the recorded deleted file.

`editor_review_app` loads full evidence and applies decisions on background workers.
`session_file_review` reconstructs complete before/after versions from confirmed
tool effects; preview hunks and truncated output cannot become a full source
document. Accepting a block incorporates its current content into the review base
and removes only that pending block; it does not write project bytes. Rejecting a
block restores its recorded original bytes after checking the current disk image,
then reloads the clean editor. Pending ranges and counts are projected again after
each decision. Both decisions are persisted separately from immutable evidence,
Git staging, journal history and chat baselines. Historical file cards keep their
recorded diff, and a later agent edit has a separate review identity. Rejecting a
rename is unavailable because it affects two paths; the disabled action explains
that limitation.

The local old OpenIDE reference informed this interaction:
`openideEditReview.ts` (`ReviewBlockWidget`, line 614 onward, and
`ReviewHeaderWidget`, line 728 onward) supplies active-block decisions in the
ordinary editor and reserves breadcrumb space. `media/openideChat.css` (line 391
onward) uses compact text actions for the block. Native Rust components implement
the behavior independently; the old Monaco widget is not embedded. The focused
review binds Ctrl/Cmd+Enter to keep the current file, Ctrl/Cmd+Shift+Backspace to
undo its pending changes, and F7/Shift+F7 to navigate changes. See
[keybindings](keybindings.md) for scope, conflict handling and reference sources.

### Using the editor

1. Open the right dock's project file list and choose a supported text file.
   Its tab shows the editable document, filename, UTF-8/line-ending status and
   cursor position. A dot and **Sin guardar** identify unsaved changes.
2. Edit and press **Ctrl+S** or **Guardar**. A save runs on the worker; a failed
   or conflicting save leaves the text available in the editor.
3. **Volver a leer el archivo** loads the disk version. If the buffer is dirty,
   an `AlertDialog` asks before discarding it; Cancel/Escape preserve the text.
4. Closing a file tab retains its unsaved document for reopening while the app
   is running. Closing the application asks before discarding dirty documents,
   and an admitted save must finish before the discard/exit action is enabled.
   Dirty drafts are persisted asynchronously in the private editor-draft store.
   Restart recovery validates metadata/checksums and preserves source text as dirty;
   it does not silently overwrite project files. A directory lease prevents two
   app instances from modifying the same recovery store.
5. Search/replace, symbols, folding, wrapping, indent options and language controls
   are available from the editor toolbar/menu. F12 opens definitions, Shift+F12
   requests references, Ctrl+Space requests completion and F2 starts a rename
   review when the connected server advertises those capabilities.
6. New file and Save As use the same scoped file worker. External changes expose
   compare/reload/save choices; missing files retain their editable draft.
7. Use gutter chevrons to fold a block, click a sticky scope header to return to
   its declaration, or press F9 to toggle a breakpoint. Open Depuración from the
   dock menu, configure an adapter and use Start explicitly; pending markers
   become verified only when the adapter confirms them.

## Zed references and boundary

The local reference checkout is pinned to
`d284cbbc87b55d659303106ced627c8612369827`. The following observations informed
the separation of concerns; implementation here uses independent types and
algorithms on the dependencies above.

- [Text buffer and snapshot](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/text/src/text.rs#L59):
  buffer text, immutable snapshots and transaction history are separate from widgets.
- [Selection-aware undo](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/editor.rs#L7956)
  and [transaction boundaries](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/editor.rs#L8742):
  undo/redo restore selections along with text.
- [Word movement](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/movement.rs#L256)
  and [Tab/Backtab](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/editor.rs#L5323):
  code-oriented word stops and selection-aware indentation informed the native
  command contracts above. Our core implements independent rope algorithms;
  it does not import Zed's language-specific classifiers or snippet implementation.
- [DisplayMap](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/display_map.rs#L215):
  physical text coordinates remain distinct from tabs, wraps, folds and other visual rows.
- [Visible-row calculation](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/element.rs#L8689)
  and [visible-line layout](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/element.rs#L3167):
  layout requests visible rows instead of rendering all document lines.
- [Versioned background parsing](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/language/src/buffer.rs#L1930):
  parse work is tied to the text version and rerun if content/grammar changed.
  Our independent Tree-sitter service follows that versioned-worker contract; it
  does not import Zed's parser service, grammar registry or language-server store.
- [Zed sticky-header refresh](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/editor.rs#L1922)
  derives enclosing outline items from a buffer snapshot and scroll anchor on a
  background task. Its [indent-guide layout](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/src/element.rs#L2379)
  works from the visible buffer range and subtracts the current scroll offsets.
  Our implementation retains these separation principles with independent caches
  and the custom `openide-ui` renderer.
- [BufferDiff](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/buffer_diff/src/buffer_diff.rs#L22)
  and [diff update](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/buffer_diff/src/buffer_diff.rs#L1978):
  a diff has explicit base/active snapshots and change capabilities. Our existing
  journal reconstruction remains its own source and algorithm.

Zed's `editor`, `text`, `rope`, `language` and `buffer_diff` manifests at that
commit declare GPL-3.0-or-later; for example the
[editor manifest](https://github.com/zed-industries/zed/blob/d284cbbc87b55d659303106ced627c8612369827/crates/editor/Cargo.toml#L6).
Those crates and their source are not copied or linked. Ropey's independent
implementation is [MIT licensed](https://github.com/cessen/ropey).

The framework implements its own buffer commands, multi-selection model, syntax
workers, visual mapping and input handling. The host owns file authority,
recovery, language-process configuration and journal provenance. These boundaries
and selected UX patterns were compared with Zed, but this is not a claim of full
Zed parity or a measured 120 Hz guarantee.

The local Code OSS reference also informed the structure controls:
[fold decorations](https://github.com/microsoft/vscode/blob/main/src/vs/editor/contrib/folding/browser/foldingDecorations.ts)
distinguish expanded/collapsed chevrons, and the
[sticky-scroll widget](https://github.com/microsoft/vscode/blob/main/src/vs/editor/contrib/stickyScroll/browser/stickyScrollWidget.ts)
keeps source lines, line numbers and fold controls aligned while reusing unchanged
headers. Its [debug icon registry](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/debug/browser/debugIcons.ts)
distinguishes verified, disabled, unverified and trigger-pending states. OpenIDE's
local pending marker has no trigger dependency; adapter verification belongs to
the active DAP session and is displayed separately. No reference
implementation is embedded or copied into the native component.

For selection decoration, Monaco's
[visible-range classification](https://github.com/microsoft/vscode/blob/main/src/vs/editor/browser/viewParts/selections/selections.ts)
compares adjacent row endpoints to distinguish flat, internal and external
corners; its [selection stylesheet](https://github.com/microsoft/vscode/blob/main/src/vs/editor/browser/viewParts/selections/selections.css)
sets a 3 px radius. The native editor uses that visual reference with its own
single-contour geometry and painter rather than DOM/CSS selection overlays.

## Validation and reproduction

Current validation results and measured runs belong in
[Native editor validation](native-code-editor-validation.md). Design documentation
does not treat historical test counts or one local benchmark as a current
performance guarantee.

Core tests cover rope/scalar/grapheme contracts, transactions, selection history,
search/replace, multiple cursors, snippets, indentation and visual mappings. UI
tests exercise actual component input, IME/clipboard ownership, caret timing,
viewport caches, wrapping/folding, diagnostics and snapshot namespaces. Language
tests use the real grammars and a framed stdio subprocess, plus opt-in tests
against an installed rust-analyzer. Host tests cover scoped file authority,
recovery, conflicts, background edit loading and explicit review before mutation.
Their fixtures use temporary local files, not live user documents.

The visual exporters use native Ubuntu/Hack font bytes and native width/height
measurements where specified. SVG fallback glyphs, raster hinting and older
approximate fixtures still prevent a pixel-exact comparison with the live native
window. The line-alignment export checks selection/caret rows, code and diff
baselines at both 1× and 2× scale. Visible-row and full-frame benchmarks remain
opt-in and should be recorded with their build profile, size and workload.

Opt-in reproduction commands (SVG/PNG exporters use synthetic data):

```sh
rtk cargo test -p openide-editor
rtk cargo test -p openide-language
rtk cargo test -p openide-ui code_editor
rtk cargo test -p agents-window editor_language_app::tests
rtk env OPENIDE_RUST_ANALYZER=/absolute/path/to/rust-analyzer rtk cargo test -p openide-language -p agents-window real_rust_analyzer -- --ignored --nocapture
rtk cargo test -p agents-window code_editor_component_visual_tests
rtk cargo test -p agents-window export_code_editor_component_visual_review -- --ignored --nocapture
rtk cargo test -p agents-window benchmark_code_editor_visible_frames_and_typing -- --ignored --nocapture
rtk cargo test -p agents-window app::code_editor_app::tests
rtk cargo test -p agents-window code_editor_host_actions_tests
rtk cargo test -p agents-window export_actual_dock_editor_and_dirty_reload_alert -- --ignored --nocapture
rtk cargo test -p agents-window export_editor_review_native_visual_review -- --ignored --nocapture
rtk cargo test -p agents-window export_conversation_stream_visual_review -- --ignored --nocapture
rtk cargo test -p agents-window export_inline_chat_editor_diff_visual_review -- --ignored --nocapture
```

Default review directories:

- `/tmp/openade-editor-review`: native-font IDE and Agents editor review, including
  breadcrumb controls and inline old/new rows, at 360/900 px.
- `/tmp/openide-code-editor-visual-qa`: eight PNGs and eight SVGs for editable,
  read-only, horizontally scrolled and diff views at 360/900 px. Override with
  `OPENIDE_CODE_EDITOR_VISUAL_DIR`.
- `/tmp/openide-editor-visual-qa`: host document-header previews at 320/360/900 px;
  `export_native_editor_visual_review`, configurable with `OPENIDE_EDITOR_VISUAL_DIR`.
- `/tmp/openide-editor-host-visual-qa`: actual dock editor at 900 px and dirty
  reload dialog at 360/900 px, with PNG/SVG pairs. Override with
  `OPENIDE_EDITOR_HOST_VISUAL_DIR`.
- `/tmp/openide-conversation-stream-visual-qa`: sixteen PNGs and their SVGs for
  eight conversation phases at 360/900 px. Override with `OPENIDE_STREAM_VISUAL_DIR`.
- `/tmp/openide-inline-diff-visual-qa`: closed/open inline chat diff at 360/900 px.
  The current export fixture writes to that directory directly.
