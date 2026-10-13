# MCP Streamable HTTP OAuth status

The harness implements an opt-in OAuth 2.1 authorization-code flow for MCP
Streamable HTTP. It follows the [MCP 2026-07-28 authorization specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)
and its [discovery rules](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/authorization-server-discovery).

`OAuthSession::discover` reads a Bearer challenge when supplied, otherwise
tries the protected-resource well-known paths. It validates the resource and
the authorization server issuer, tries OAuth and OpenID Connect metadata in
specification order, and requires advertised PKCE `S256`. Authorization uses
random state and a PKCE verifier, binds a listener to `127.0.0.1`, checks the
callback Host, Origin, state, issuer (`iss`) and redirect path, and sends the
canonical MCP `resource` parameter in authorization, token, and refresh
requests. Requests have size and time limits, do not follow redirects, and
can be cancelled. OAuth issuer and endpoint URLs require HTTPS; HTTP loopback
authorization servers are allowed only in unit tests.

`TokenStore` is the storage boundary. `MemoryTokenStore` keeps tokens only for
the current process and is **not durable secure storage**. On Unix,
`FileTokenStore` keeps each grant in a file of its own under `mcp-oauth/` in
the application's data: the directory is `0700`, each file `0600`, written
whole and renamed into place, and every read or change holds one `flock`, so a
refresh in one process cannot interleave with another's. A file that others
can read, that is not a plain file, or that names another resource, issuer or
client is refused. Its grant revision changes with each authorization and not
with a refresh, which makes its grants eligible for resume identity. This is
private-file storage, not an encrypted vault: whoever can read the user's
files can read the tokens, as with the provider credentials the app already
keeps the same way. On Windows,
`WindowsCredentialTokenStore` stores a generic credential for the current user
with local-machine persistence. Its target name contains a SHA-256 hash of
resource, issuer, and client ID; the token blob is limited to 2560 bytes.
Tokens are never written to the agent journal.
New authorizations also store a random, non-secret grant revision in that
credential blob. Refresh preserves it only when the previous credential still
matches under a named Windows mutex; reauthorization changes it. The native
runner binds the revision, resource, issuer, client ID, scopes, authorization
metadata, HTTPS MCP endpoint, server handshake and tool catalog in a durable
digest. It checks the revision again before every tool POST. Older vault blobs
without a revision remain usable for live calls but cannot certify an
interrupted call for automatic resume until the user reauthorizes. See
`docs/native-mcp-resume-identity.md`.
An `invalid_grant` refresh response deletes the old credential only if it
still matches the tokens that were refreshed; it cannot erase a newer
authorization saved by another process meanwhile.
Credential Manager operations run on blocking worker threads. Once a Windows
vault write starts, cancelling the awaiting OAuth operation cannot roll that
write back. The app waits for the authorization worker to stop before deleting
the previous credential. It deletes that credential when saving a changed
resource URL, issuer, or client ID, or when removing a server. Disabling a
server or editing scopes retains the credential. If deletion fails, the app
keeps the original registration and reports the error.

The app persists only the public client ID, exact HTTPS issuer, and
requested scope names in MCP settings. Its Settings page starts authorization,
shows the temporary browser URL and progress, and opens that URL only when the
user clicks the button. A bounded, read-only Streamable HTTP GET collects a
`WWW-Authenticate` Bearer challenge from a 401 before metadata discovery; a
405 or response without a challenge uses the well-known metadata paths. The
loopback callback saves tokens in Credential Manager on Windows and in the
file store on Unix. At runner startup an
OAuth server requires a stored usable token; otherwise it reports that the user
must authorize in Settings. No auth flow is launched by the agent run.

`McpHttpClient::connect_with_oauth` requests or refreshes a token before each
HTTP dispatch. It rejects a simultaneous bearer token from the environment.
A `tools/call` that receives HTTP 401 after dispatch stays effect-uncertain;
the client does not retry that POST automatically.
If OAuth refresh fails or the grant changes before dispatch, the call stops
before the POST. A successfully refreshed access/refresh token pair retains
the grant revision, while a changed scope set invalidates the recovery digest.

The explicit Settings inspector reuses this authorization flow with an
observing `TokenStore` wrapper. It delegates every storage operation to the
existing store and collects access and refresh token values only in bounded
memory (256 distinct values, at most 8 MiB) for report sanitization. Tokens
loaded, saved, refreshed or observed while closing the connection are checked
before publishing metadata or its hash. A report containing an observed
credential is rejected; diagnostics redact those values. Exceeding the
observer limit suppresses both metadata and raw diagnostics. This wrapper
adds no token persistence and changes no grant or refresh authority.

`oauth_store_observer_preserves_refresh_cas_and_checks_new_tokens_before_report_hash`
uses the real `MemoryTokenStore` to verify refresh compare-and-swap, grant
revision preservation, stale-write rejection and credential collection across
a simulated closing boundary. It does not exercise an OAuth HTTP refresh,
browser authorization or Windows Credential Manager. Desktop HTTPS endpoint
requirements remain enforced; the harness's test-only loopback exception is
not enabled when that harness is a dependency of the desktop tests.

Remaining work:

- An encrypted vault on Unix (Secret Service, Keychain) in place of private
  files, and a store for macOS's own keychain. The Windows adapter has format
  and key tests but no automated test that writes to a user's vault.
- Reauthorization or explicit revocation after a runtime 401. A dispatched
  `tools/call` must remain effect-uncertain and must never be retried
  automatically. The app deletes credentials when a registration is changed
  or removed, but does not yet offer a standalone revoke action.
- Explicit authorization-server selection when protected-resource metadata
  lists several issuers. The app currently uses a pre-registered client ID
  bound to the exact configured issuer.
- Dynamic Client Registration (DCR) remains pending. Current registration is
  either a pre-registered public client ID bound to an issuer or a hosted
  HTTPS client ID metadata document. The app must provide that client ID;
  this module does not host the metadata document.
- Step-up UI for HTTP 403 `insufficient_scope`. `begin_step_up` parses the
  Bearer scope challenge and unions it with previously granted scopes; the
  app must pass that header, ask the user to authorize the additional scopes,
  and avoid replaying an effect-uncertain tool call.

No token value, authorization code, PKCE verifier, or callback query should be
logged or persisted in settings, telemetry, or a run journal.
