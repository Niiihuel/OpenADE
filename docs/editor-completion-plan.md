# Native editor completion

This work extends the existing `openide-editor` / `openide-ui` implementation.
The reference is Zed's separation of text, display, project and language services;
the implementation remains native to OpenIDE.

## Integration contracts and ownership

- Editor core/component: Unicode search and atomic replacement, multiple
  selections, rectangle selection, editing commands, pairs, snippets, line
  endings, visual wrapping and folds. Host actions use a shared controller.
- Language services: versioned Tree-sitter analysis, bounded syntax workers,
  symbols and folds; real LSP stdio with bounded queues and revision checks.
- Files/recovery: asynchronous durable drafts, external-change checks, conflict
  views, create/save-as without implicit overwrite, journal provenance.
- Host/UI: search and goto controls, navigable breadcrumbs and outline, language
  actions, contextual menus, settings, tab identity, bounded retained views.

## Completion checklist

- [x] Bound retained editor/diff state and syntax workers; long-line stress cases.
- [x] Context menu targets clicked code and routes editing/language actions safely.
- [x] Duplicate tab names are disambiguated and full paths are available.
- [x] Drafts survive restarting; saved/discarded drafts cannot reappear.
- [x] External changes refresh clean documents and preserve dirty documents.
- [x] Create, save-as, deleted-file recovery and conflict comparison work.
- [x] Find/replace, options, next/previous, replace-all and goto line work.
- [x] Multicursor, rectangular selection, line commands, pairs and snippets work.
- [x] Wrapping/folding preserve text coordinates, selection and undo.
- [x] Breadcrumb folder navigation, copy path, symbols and outline work.
- [x] Parsing and LSP completion/hover/definition/references/rename/format/actions
  are connected to actual buffers; stale responses cannot change newer text.
- [x] Diagnostics are visible; language/tab/line-ending settings affect editing.
- [x] Deterministic host end-to-end tests cover each user flow with real files
  and workers; LSP transport tests include an actual installed server.
- [x] Native desktop smoke test and visual review cover compact/wide layouts.
- [x] Optimized build, regression suites and performance evidence are recorded.

## Verification policy

End-to-end host tests enter through input or rendered controls, exercise the real
controllers/workers, and assert file/buffer outcomes. They are distinguished from
core unit tests and native desktop/GPU smoke tests. Source files, providers and
user conversations are not fixtures. Temporary workspaces isolate test writes.
Performance reports identify whether they measure component CPU, full-frame CPU,
GPU or end-to-end input latency; no FPS guarantee is inferred from a unit test.

## Recorded outcome

See [validation](native-code-editor-validation.md) for the final suite, real LSP,
native desktop run and component CPU measurements, and
[design](native-code-editor-design.md) for exact capabilities and remaining
boundaries. The completed checklist describes this implementation scope; it is
not a claim that every feature of Zed or every application frame was benchmarked.
