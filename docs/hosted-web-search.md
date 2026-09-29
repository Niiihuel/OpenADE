# Hosted web search in the native Responses backend

The native Responses backend can offer provider-hosted `web_search` as an opt-in tool. It follows the tool shape in the pinned Codex reference at `references/codex` commit `33a0f766a647208b471cfbcad889c67fd324ee04`, especially `codex-rs/core/src/tools/hosted_spec.rs`. The [official OpenAI web search guide](https://developers.openai.com/api/docs/guides/tools-web-search) documents `web_search_call` output items, URL citation annotations, domain filters, and `include: ["web_search_call.action.sources"]`.

## Transport contract

Construct `HostedWebSearchConfig::new(mode)` and pass it to `ResponsesBackend::with_hosted_web_search(config)`. Search is disabled unless this method is called. The model decides whether to use the hosted tool. Local function tools may be present alongside it.

| Mode | Responses tool fields |
| --- | --- |
| Cached | `type: web_search`, `external_web_access: false` |
| Indexed | `type: web_search`, `external_web_access: true`, `indexed_web_access: true` |
| Live | `type: web_search`, `external_web_access: true` |

Optional `context_size`, allowed or blocked domains, and approximate `location` map into the hosted tool object. Domain lists are limited to 100 entries; a config cannot contain both allowed and blocked lists. The default `include_sources: true` adds `web_search_call.action.sources` to `include`. The request profile fingerprint covers the effective config, so native resume rejects a turn if its hosted search settings change. The API key is not included in the fingerprint.

## Results and citations

Text deltas still stream immediately. The final Responses output preserves each original `web_search_call` item and each `output_text.annotations` array in `provider_output_items`. Native session replay stores those raw items, including reasoning state, for stateless continuation.

`ResponsesResult::web_search_calls()` and `ResponsesResult::web_citations()` expose typed views. For live or reopened transcripts that have only durable `AgentMessage.provider_output_items`, use the equivalent public functions:

```rust
let calls = openide_harness::hosted_web_search_calls(&assistant.provider_output_items)?;
let citations = openide_harness::hosted_web_citations(&assistant.provider_output_items)?;
```

Each `HostedWebCitation` records the output item and content part indices, the provider's start/end offsets, URL, and optional title. The parser requires HTTP or HTTPS URLs. It does not rewrite assistant text or interpret the provider's offsets as Rust byte indices. The transcript UI must present citations as visible, clickable links, as required by the [OpenAI guide](https://developers.openai.com/api/docs/guides/tools-web-search#output-and-citations).

## Recovery and current limits

After an HTTP success with hosted search enabled, an SSE disconnect is terminal even before the first text delta: the provider may already have searched, and resending the request could duplicate that work. Pre-response connection failures and retryable HTTP statuses retain the bounded transport retry policy. Replaying a completed response uses the preserved raw output items.

The current native adapter handles text search. It does not request image search results or expose incremental typed search-status events through `ModelBackend`; the completed search calls and citations are available from the output items. Provider availability and support for `indexed_web_access` depend on the selected endpoint and model. Local HTTP tests cover the wire request, streaming text, citation/source extraction, replay, and the ambiguous-disconnect boundary; they do not make a live OpenAI API request.
