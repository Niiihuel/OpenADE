# Changelog

Every release of openADE, newest first. The versions before 0.2.0 were tagged
afterwards, on the last commit of each day of work; their `Cargo.toml` still
says 0.1.0.

## Unreleased

- Agents over the Agent Client Protocol: any agent of the public ACP
  registry installed on this machine (Grok Build, Gemini CLI, Copilot, Cursor,
  Qwen Code, opencode and others) starts a task from the home screen and
  answers in the ordinary conversation, with streamed text and reasoning,
  tool rows, plans, approval cards and Stop; its session reopens after a
  restart when the agent supports it
- Settings → External CLIs lists every agent of the ACP registry with how it
  runs here: the user's own install, a package that `npx` or `uvx` fetches
  (offered once switched on), or the vendor's build, which Install streams
  to disk, checks against its SHA-256 when the registry publishes one, and
  Remove deletes (Google Antigravity among them)
- The composer picks an ACP agent's own models and reasoning effort; the
  agent starts when its task opens so they are there before the first
  message, and the choice is kept for the next tasks of that agent
- A Graph tab in the dock draws the workspace's files, and their imports,
  calls and type uses, as a force-directed graph: hovering a file lights up
  its neighbours, nodes can be dragged, the view zooms and pans, and a click
  opens the file. Its floating controls filter by path and link kind, color
  groups of files, show only what surrounds the open file, play the project's
  growth as a timelapse and tune the forces; they are kept in `graph-view.json`
- `openade-graph`: an incremental index of the workspace's code that agents
  query over MCP or the command line instead of rereading files, with a hook
  that points CLI agents to it before they grep or read a large file

## 0.3.0 — 2026-10-16

- Settings reach every module: browser automation with per-site permissions,
  subagents, MCP servers with their scopes and tool selection, appearance with
  fonts and custom themes, and brand icons for providers
- Editor: line numbers, indent guides, current line highlight, visible
  whitespace, a column ruler, caret shape and blink, trimming trailing
  whitespace on save, and Vim mode, all in Settings > Editor
- Usage: connected accounts first and the rest in one compact list, every
  limit read as used, tasks by agent as ranked bars with the agents' names,
  and a conversation's context shown as unmeasured instead of "~0%"
- A Linux archive with its embedded Chromium and an installer, attached to
  each release from now on
- The English interface no longer shows Spanish in joined texts, counts and
  times
- Settings reachability tests no longer fail on a loaded machine
- limn pinned to `90d4408` (limn#70: editor view options, trailing whitespace
  trimming, whole chart ticks)

## 0.2.0 — 2026-10-13

- Own providers and CLI profiles: added, edited and removed from Settings; an
  own provider can be chosen in the composer and run a turn
- A light theme chosen from cards, and the interface in English or Spanish
- Automations: a list with its detail, more schedules, a model per task, a run
  history that goes with its task, and a warning on quit
- Marketplace and add-ons: skills, plugins, MCP and sources in one section;
  installed skills in the slash list and updates from the user's own sources
- MCP: servers imported from Codex and Claude Code, OAuth on Linux, headers from
  environment variables, each server's last connection, and a failing server no
  longer stops the turn
- Browser: runs on embedded Chromium (CEF) alone, with tabs, a visible agent
  pointer, per-site permissions, downloads, file uploads, the clipboard,
  locators by text, label, placeholder and test id, and waits for an address
  and a load
- Git: a pull request form, reviews of a branch or another origin, viewed
  changes, discarding one block, pull request checks and a commit message
  written by a model
- Conversation: a durable next-turn queue with attachments, waiting messages
  that can be edited, a summary of each turn, a rail of turns and a subagents
  panel
- Dock: a new-tab page with suggestions, and a maximize that covers the
  conversation
- An IDE status bar that reads from the branch, and the name openADE
- Hooks: a hook that exits without reading its input completes, instead of
  leaving its call uncertain
- MIT OR Apache-2.0 license, a Sponsor button and continuous integration on
  GitHub Actions

## 0.1.4 — 2026-10-12

- Paused conversations and automations continue by themselves off screen
- Questions to the user take images in the answer, and a conversation that
  waits for one is marked
- The Activity log is a timeline, with each call's detail under its line
- Agent commands are terminal tabs, with a live preview of a server they start
- Image generation with the user's OpenAI connection, asking every time
- Vision: a model is given the images its tools return
- The environment panel and the context window use limn's cards
- An image viewer in the window, a right dock that maximizes, and Mermaid
  flowcharts and sequence diagrams in the chat and in the preview
- CEF: drop-down lists, modifier keys, the page's cursor, Spanish text entry
  and smooth wheel scrolling
- An external CLI drives a browser of its own through the bridge

## 0.1.3 — 2026-10-11

- An opt-in embedded Chromium (CEF) for the project browser
- A Rust browser driver over the DevTools pipe, with no Node, npm or
  Playwright, and Playwright-style tools with element refs, console, network,
  evaluate and screenshots
- Dictation off Windows: whisper.cpp transcribes the microphone locally
- Mermaid flowcharts drawn natively in the chat
- Edit a message while its turn runs, rewind, and resume an interrupted turn on
  its own model
- A redesigned source control sidebar: branch and sync, a commit composer and
  grouped changes
- Debug and extensions panels, model metadata and the context ring
- Started commands run in a Unix terminal that can be shown and typed into

## 0.1.2 — 2026-10-10

- The interface moves to the limn UI framework, section by section
- An Activity log by turn, with filters, a detail by kind and how long each
  call took
- Codex and Claude Code subscription limits, in the IDE footer and the sidebar
- Approvals answered from their transcript card
- Task groups with a heading row, and a view of archived tasks

## 0.1.1 — 2026-09-29

- A native provider catalog
- A broader native agent runtime and a polished Rust desktop UI

## 0.1.0 — 2026-09-28

- First version: the native Rust agents window, with a shared drop-down menu,
  composer and buttons
