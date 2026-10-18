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
- ACP agents get the composer's attachments: files and folders as paths
  they open themselves, and images inline to an agent that takes them,
  shown on the message as for the native agent, or as paths to one that
  does not
- An ACP agent that asks to sign in does so on the first message rather
  than when its task opens; the conversation says it is waiting for the
  sign-in, and Stop ends the wait
- The ACP registry list in Settings has a search field
- A task's turn goes on when another task is opened or created, instead of
  having to be stopped first: its row wears the clock, what it asks waits for
  an answer, its end is announced and leaves the task unread, and opening it
  again shows the live turn with its Stop; the message of a turn that failed
  off screen goes back to that task's draft. Quitting with such turns asks
  first
- A command prefix allowed in Settings → Comandos runs without asking while
  the agent is in «Pedir aprobación»; a command's approval card offers
  «Permitir siempre» for its prefix, which saves that rule and allows the
  call for the session
- A CLI task starts its CLI with the permissions chosen in its header — ask,
  read-only (plan), or allow all — through each CLI's own flags, and starts
  it again on the CLI's own conversation: Claude Code under the session
  openADE names for it, Codex and opencode under the session found after
  their first start
- ACP agents take the composer's Plan mode, through their own plan mode when
  they have one, and its presets: «Permitir todo» answers their requests and
  «Sólo lectura» refuses their edits and commands; a turn the app was closed
  in the middle of is offered to go on
- ACP agents wear their company's mark on the home screen, the new-task
  menu, the composer and the task list: Grok Build is Grok's, Gemini CLI is
  Gemini's, and so on for every agent the brand catalog names exactly
- The context panel no longer names a model when there is none to send to:
  the saved id was shown as if it were connected
- Settings → Conexiones connects OpenAI, Anthropic, xAI, DeepSeek, Mistral
  and Groq with an API key, as it did OpenRouter, OpenCode, NVIDIA, MiniMax
  and Z.ai: the key is kept privately on this machine and the native agent
  uses it, with no environment variable to set
- The IDE works in several folders: the explorer's folder name opens the
  folders the IDE has worked in, any of them becomes the active root, a new
  one is added from disk, and the others sit under the tree as roots to
  switch to; tasks started from the IDE take the active folder. The list is
  kept across restarts
- The key connection form for OpenAI, Anthropic and xAI says where each
  company's subscription is used instead: the Codex connection, Claude Code
  and Grok Build
- The composer's model selector wears a generic mark; the providers keep
  theirs inside the list
- A model's details are chips for what it takes and does and tiles for its
  limits and prices, sized to their content; a provider not yet connected
  shows one row inviting the connection instead of an empty catalog and an
  error
- The API key of a provider connected by key enables Conectar as soon as
  it is typed; before, only OpenCode's and NVIDIA's did
- The usage card says the figure and its window on one line, the renewal
  beside the window's name, what is known of the measurement in one line,
  and its button in the header
- The Codex connection registers itself with ChatGPT as openADE: a record
  from before the rename registers again on the next sign-in
- The Graph tab opens with the whole graph in view and keeps it so until
  the camera is moved
- A notice about an MCP server left out no longer ends in two periods
- An MCP server with more tools than the model's catalog takes (128 with the
  local tools) is no longer left out of the turn: the tools that fit are
  advertised and the agent finds the rest with `search_mcp_tools` and calls
  them with `call_mcp_tool`, which asks as any MCP tool does; Claude Code and
  Codex defer the tools of a large server the same way
- A provider that refuses the request for its models says why, in its own
  words, after the HTTP status: before, the connection form said only
  «HTTP 400»
- The connection form is as tall as its content, with «Obtener clave de API»
  beside the key's label; before, the key forms had empty space under the
  note
- A modal whose content fits no longer shows a scrollbar: a layout rounding
  error of millionths of a pixel counted as overflow (limn#77; limn is
  pinned to `bb10b72`, limn#81 on top of it, which adds the toast's icon,
  buttons and progress)
- A Graph tab in the dock draws the workspace's files, and their imports,
  calls and type uses, as a force-directed graph: hovering a file lights up
  its neighbours, nodes can be dragged, the view zooms and pans, and a click
  opens the file. Its floating controls filter by path and link kind, color
  groups of files, show only what surrounds the open file, play the project's
  growth as a timelapse and tune the forces; they are kept in `graph-view.json`
- `openade-graph`: an incremental index of the workspace's code that agents
  query over MCP or the command line instead of rereading files, with a hook
  that points CLI agents to it before they grep or read a large file
- A file of the project named in an agent's reply as inline code
  (`src/app.rs`, `src/app.rs:42`, `src/app.rs#L42`) is a link that opens it
  in the dock, at that line
- Files dragged from another application and dropped anywhere on a task's
  page are attached to the composer, whose border lights up while they are
  over the window; on Wayland they arrive through limn-dnd
- The sidebar filters tasks by title or project as you type, and offers to
  search the words inside the tasks in the command center
- A short chime plays with each notice (Settings → Notificaciones → Sonido,
  on by default), and a desktop notice also asks the window manager to mark
  the window until it is seen
- A Claude Code or Codex task shows in the sidebar whether its CLI is
  working (clock) or waiting for an answer or an approval (chat mark), and
  leaves the task unread with a notice when its turn ends off screen. Claude
  Code reports through hooks passed with `--settings`, which write to
  `OPENADE_CLI_STATUS`; Codex through its OSC 9 notifications, which the
  terminal now reads
- Restore points: before each turn of any agent (native, ACP, headless or
  terminal CLI) the project's working tree is committed under
  `refs/openade/checkpoints/<task>/<n>` from a private copy of the index, so
  HEAD, the index and branches stay as they are. The history button in a
  task's header lists them and puts the files back as they were before a
  turn, ignored files aside; the state just before is kept, and the same
  menu undoes the restore. Refused while an agent works or the editor holds
  unsaved changes; the last 50 points of a task are kept, and deleting the
  task removes them
- A CLI connected to the bridge can delegate: `delegate_task {agent, prompt}`
  hands a task to openADE's own agent, an installed ACP agent or a headless
  CLI in the same project. It starts as an ordinary task, runs off screen
  with its row in the sidebar (where the person can open it, answer what it
  asks or stop it) and its final answer goes back to the caller. Settings →
  External CLIs → Delegar tareas switches it on (approval as for the other
  tools); at most four run at once. The bridge raises the CLIs' MCP tool
  timeouts to an hour (Codex `tool_timeout_sec`, Claude `MCP_TOOL_TIMEOUT`)
  so a long task's answer is not dropped
- The review panel merges (squash or merge commit, after a confirmation),
  approves and comments on the branch's pull request
- A task can watch its pull request: every 90 seconds openADE reads its
  checks, comments, diff comments and reviews, and notifies when checks start
  failing, someone else writes, or it is merged or closed (Settings →
  Notificaciones → Pull requests vigilados). Optionally the news goes to the
  task's agent: typed into its CLI's terminal once the CLI is idle, or sent as
  a turn of the native or ACP agent, off screen or queued behind the running
  one; watches are kept in `pr-watch.json`
- Updates: openADE looks for a new release every six hours, and an install
  made from the Linux archive downloads it in the background, checks its
  minisign signature and puts it in place when it restarts or quits. Other
  copies say a new version exists and link to it. Settings > General has the
  version, a check button and the switch to turn it off
- Notifications stack up to three cards with an icon for their kind, buttons
  and a progress bar; an update's card stays until it is dismissed
- Release archives are signed; the `.minisig` beside each one is what the
  updater verifies
- Releases also build a Windows zip and a macOS (Apple Silicon) app bundle,
  signed the same way, without the embedded browser and without platform
  code signing; their updater points to the release instead of installing
  it. They are new and unproven, so a failure of theirs does not stop the
  Linux release
- A turn's menu forks the task from there with any agent: openADE's own,
  an installed ACP agent or a CLI. The new task, in the same project, gets
  the conversation up to that turn as text (its first message and as much of
  its end as fits in 16 KB, tool use as one line per turn, marked as history
  and not instructions): written in the composer for the person to add what
  to do, or typed into the CLI's prompt once it has started
- "Nueva tarea en un worktree" in the composer's environment menu creates
  `<repo>.worktrees/<id>` on a new branch `openade/<id>` beside the
  repository and opens a new task's draft there; the repository's
  `.openide/setup` (one command per line, `#` comments) runs in a terminal
  that goes with the task. Deleting the worktree's last task removes the
  worktree if nothing in it is uncommitted; its branch stays

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
