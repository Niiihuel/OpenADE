# Native model route provenance

`ModelRouteCommitted` version 1 records the configured route used by a native
turn before its first `ModelRequestStarted`. This is durable metadata for exact
provider and model identity; it does not infer a provider from a model name or
from the currently selected UI account.

## Stored contract

The event contains `version`, `provider_id`, `model_id`, `endpoint_digest`, and
`request_profile_digest`. Provider and model IDs are exact strings, nonempty,
without control characters, bounded to 128 and 512 UTF-8 bytes respectively.
The endpoint identity is bounded to 16 KiB and hashed with SHA-256 in the
`openide-native-model-route-endpoint` domain, version and byte length included.
It is not normalized: distinct endpoint strings have distinct identities.
Endpoint URLs, API keys, headers, and credential values are not stored in this
event.

The request profile is the model backend's stable 32-byte fingerprint, stored
as canonical lowercase hexadecimal. Its contract includes the wire API and
the request configuration relevant to recovery. The existing Chat Completions
and Responses fingerprints have separate wire API domains; model capabilities,
reasoning configuration, output caps, and other supported request options stay
in their existing backend fingerprints. The route does not replace the
`ModelContextCommitted` digest of the assembled native prompt and context.

Both hashes must have exactly 64 lowercase hexadecimal characters. Memory and
JSONL stores reject invalid versions, IDs, or hash shapes. Loading a complete
invalid JSONL event fails visibly without rewriting or discarding the event.
Native replay validates the event's session, sequence, admission model, and
position. There is at most one route per turn, after `ModelContextCommitted`
and before its first model request. Metadata from earlier turns remains in the
journal even when the active replay fields reset on completion or a checkpoint
covers those turns.

## Runner and Resume

Leased native `AgentRunner` commits the model context and exact route before
allowing the first request. A route write failure disconnects the run without
dispatching that model request or a tool. A route can therefore be the last
durable event when a process stops before the request begins.

`ResumeEngine::inspect` compares a saved active route with the host's configured
provider ID, exact backend model ID, endpoint identity, and current stable
request profile before appending recovery events or dispatching requests,
hooks, or tools. A changed provider, endpoint, or profile reports
`ModelRouteMismatch`; an incompatible exact model also remains subject to the
existing `ModelMismatch` admission check. The request path repeats the route
check before it writes events. Matching recovery retains the original route
without duplicating or replacing it.

The host supplies `NativeRuntimeIdentity`; a generic `ModelBackend` does not
expose its endpoint for independent attestation. Hosts must pass the actual
configured backend route. This engine check occurs after the host constructs
the backend. Preventing a host credential lookup before construction needs a
host check at that boundary; the engine does not claim to guard earlier host
credential reads.

Legacy journals may omit the route and remain readable under the existing
context checks. A retry of a legacy request already recorded as started does
not receive retrospective route metadata. Recovery stopped before the first
request may commit the current route before issuing that first request. A UI
must label missing historical provenance as unknown and must not attribute it
to the provider or model currently selected in Settings. Unleased runners do
not claim this native durable contract.

## Verified behavior and limits

On Linux, the following tests verify this contract:

- Three `openide-core` `model_route_snapshot` tests validate exact arbitrary
  provider/model IDs, bounded private hashes, invalid ordering and identity,
  legacy absence, real store reopen, and corrupt complete JSONL rejection.
- Three `ResumeEngine` `model_route_snapshot` tests reject changed provider,
  model, endpoint, and profile without journal mutation or dispatch; reopen a
  crash after route commit and before the first request; and keep legacy retry
  provenance absent.
- The runner's `prepared_identity_is_synced_before_first_model_request` checks
  the exact route and its durable order. The injected route journal failure
  test verifies no request or tool and the same valid prefix after reopen.
- `native_http_cycle` exercises actual HTTP/SSE Plan, Build, tools, uncertain
  effect recovery, compaction, checkpoint and store reopen with exact saved
  routes. The current HTTP backend rejects changed routes and request output
  caps before recovery effects. Its subprocess acceptance includes a real
  process exit immediately after `route_committed`, before the first request.

These fixtures use local HTTP servers and portable Rust processes. They do not
verify a live provider account, credentials, provider catalog freshness, every
platform's process behavior, or the correctness of arbitrary third-party
backend fingerprint implementations. A context UI reading this provenance
still needs validated journal/checkpoint replay and session/project scope; the
event alone does not describe token usage or the full model message payload.
