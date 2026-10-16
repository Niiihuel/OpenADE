# Local MCP settings scopes

MCP Settings edits one local scope at a time. `Usuario` keeps the existing
`<data-root>/mcp-servers.json` version 1 document and applies to every project.
`Este proyecto` keeps another version 1 document under
`<data-root>/mcp-projects/<workspace-key>/mcp-servers.json`. The key is SHA-256 of
`openade-mcp-project-v1\0` and the platform's encoded bytes of the canonical
workspace path. A symlink to the same workspace resolves to the same scope.

Project preferences belong to this local profile. They are not repository
files, are not transferred when cloning, and do not automatically enable a
server supplied by a repository. Existing user settings are loaded without
migration or rewriting. An unavailable workspace retains user-only loading;
editing Project requires an existing directory.

The effective registration starts with User, then replaces each matching ID
with the Project registration. A disabled Project entry still replaces an
enabled User entry. Removing the Project entry reveals the User entry again.
The selected scope and its precedence are described in Settings. Saving checks
the current workspace's effective namespace and the existing limit of 16
servers. Runtime discovery and native context use the effective registration;
the editor, explicit inspection, and authorization use the selected scope.
Installed plugins keep their separate registry and existing collision checks.
Each row reports its last explicit inspection, rather than a live connection.
Runtime failures remain in the conversation journal: an ID-only notice cannot
identify which User or Project registration was used and must not overwrite the
inspection status of another registration. Settings also marks overridden User
rows and summarizes the effective configuration for the next turn.

Changing scope or workspace clears the previous inspection and authorization
view. An import worker is invalidated, including a change from A to B and back
to A; it cannot apply its previous result. Results also require the original
canonical workspace and unchanged saved registration. A worker only reads
configuration files and never starts a server, calls a tool, or authorizes an
account. Imported servers remain disabled until the user reviews and enables
each registration.

User imports inspect Codex's `~/.codex/config.toml`, the global `mcpServers` in
`~/.claude.json`, and `~/.cursor/mcp.json`. Project imports inspect the current
project's entry in `~/.claude.json`, `<workspace>/.mcp.json`, and
`<workspace>/.cursor/mcp.json`. These original files are never changed.
JSON sources currently require valid JSON; JSONC comments are reported as a
format error. Files must be regular UTF-8 files of at most 8 MiB, reads are
bounded on the opened handle, and Unix opens reject FIFO substitution without
waiting for a writer. A source list processes at most 1024 entries; at most 32
candidates are kept before removing already known IDs and applying the scope's
remaining capacity. Read and parse failures are reported without source lines
or parser excerpts, since those can contain credentials.

Imports retain literal program arguments and valid relative working
directories. They copy environment variable names, not literal environment or
header values. Cursor headers using an exact `${env:NAME}` reference become
environment-backed headers, and `Bearer ${env:NAME}` becomes an
environment-backed bearer token. Legacy HTTP+SSE entries, unsupported paths,
malformed arguments, and registrations that fail current validation are skipped.
An imported value still needs review; remote OAuth registrations are not inferred.

OAuth credentials remain identified by resource, issuer, and client ID rather
than by Settings scope. After an edit or removal commits successfully, cleanup
removes the old local credential only if no replacement row, User entry, or saved
project entry references the same key. Disabled and overridden entries still
count as references. The scan is capped at 1024 directory entries; an unreadable,
corrupt, or excessive registry conserves the credential and reports the cleanup
failure after the successful Settings save. Cleanup does not contact a remote
revocation endpoint.

A missing or escaping working directory fails its server's startup without
preventing other enabled servers from serving the turn. Discovery still freezes
the native tool catalog for the turn and preserves the existing resume rules.
HTTP header values resolved for an explicit inspection are added to the same
in-memory secret observer as bearer and OAuth tokens, so reflected values cannot
be retained in the inspection snapshot, hash, or diagnostic text. No extra token
store or credential persistence is introduced by scopes.
