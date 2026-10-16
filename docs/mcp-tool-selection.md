# MCP discovery and model tool selection

The MCP Settings page owns the native MCP view. Marketplace's former MCP link
opens that Settings page. The list
shows each saved server's identity, inspection status, transport and activation
switch. Opening its details does not connect it. Inspection, editing,
authentication and tool selection are available in that detail view.

Scope and reload controls stay in a compact toolbar. The tools are the main
detail content; connection paths, installed-package metadata and complete
inspection JSON remain available in a collapsed technical section. Technical
values wrap inside padded rows, and errors retain their complete diagnostics.
The SVGL icon preference remains separate from server configuration and tool
permissions.

Inspection and model advertisement have different budgets. Discovery retains
complete tool metadata, including output schemas and annotations. It remains
bounded to 8 MiB per transport message, 2 MiB of tool definitions in the entire
catalog, 512 tools, and 64 pages. The aggregate catalog bound also bounds every
individual definition. Input schemas advertised to the model retain their
separate 64 KiB per-tool limit, 2 MiB combined schema limit, and 128 tools across
local, window-owned browser/image, and external MCP tools.

The installed REA 6.4.0 was measured using only `initialize`,
`notifications/initialized`, and `tools/list`. No `tools/call` or target analysis
was sent. It returned 170 tools in one 1,954,727-byte response, with 1,954,512
bytes of definitions and 365,646 bytes of input schemas. Its largest definition,
`compare_web_captures`, was 69,778 bytes while its input schema was 57,175 bytes.
Only that complete definition exceeded 64 KiB; no input schema did. Applying
the model input-schema threshold to complete discovery metadata caused the
inspection failure. The measurements and hashes, without full tool contents,
are in `target/mcp-catalog-diagnostics/rea-catalog-summary.json`.

The patched native inspection controller was also checked against that real
local executable with an explicit opt-in probe. It successfully retained all
170 tools, negotiated protocol `2025-11-25`, preserved the 69,778-byte definition
and complete technical JSON, then confirmed shutdown. It started no agent turn
and sent no `tools/call`. The bounded summary is in
`target/mcp-catalog-diagnostics/real-rea-inspection.json`; the integration probe
remains ignored in the default suite because it requires that local installation.

An explicit selection is stored separately in `mcp-tool-selections.json`, beside
each scope's registration file. The version 1 document maps registration IDs
to their configuration digest and selected raw MCP tool names. The digest
includes the registration except `enabled`, so toggling preserves the selection.
Editing endpoint, program, arguments, environment configuration, authentication
registration, or working directory requires another review. A stale selection
fails closed instead of becoming All. Tokens and resolved environment values
are never included in this document.

An absent selection means All. An empty selected list means None. Named
selections are explicit and bounded; no automatic truncation occurs. A Project
registration uses only its Project selection, including All, and never inherits
a User selection merely because the IDs match. Discovery and inspection always
receive the complete catalog. Runtime filters only after complete discovery and
requires every selected name still to exist.

Selections are loaded into the turn's preparation snapshot. Window-owned tools
are composed before external server admission, so the same model budget includes
local tools, browser/image tools, and external tools. Each enabled server is
admitted as a complete unit in registration order. A server that fails selection,
schema/name validation, or the remaining advertisement budget is closed and
reported unavailable; local tools and previously admitted servers continue.
The reason directs the user to inspect and save a compatible explicit selection.
This does not claim the full inspected catalog fits every model request.

The runtime identity hashes complete discovered metadata, the selection mode and
names, and the exact advertised model catalog in addition to existing server
identity checks. Changes to unadvertised metadata or to a selection therefore
invalidate pending-call resume. Unverified identities remain unverified, and
catalog-change invalidation, permission checks, cancellation, and effect-uncertain
no-replay rules are preserved. A tool that was discovered but not selected has
no executor route and cannot be dispatched through the frozen native catalog.
