# MiniMax and GLM / Z.ai native routes

Reviewed on 2026-10-06 against the providers' official documentation. These
connections use the existing native Chat Completions harness; no external CLI
or browser bridge is involved. They target the **general API**, not a Coding Plan.

- `minimax`: `https://api.minimax.io/v1`, Bearer `MINIMAX_API_KEY`.
  The official authenticated `GET /models` operation supplies account-visible
  IDs. Its documented response does not declare token limits or tool capabilities,
  so those fields are not invented from a model name.
- `zai`: `https://api.z.ai/api/paas/v4`, Bearer `ZAI_API_KEY`.
  The published API has no model-list operation. OpenIDE keeps reviewed public
  model IDs locally and saves a supplied key without sending a `/models` probe.
  A saved key is not a successful authentication check; account access is checked
  when the user submits an inference request.

Official sources: [MiniMax OpenAI compatibility](https://platform.minimax.io/docs/api-reference/text-openai-api),
[MiniMax model list](https://platform.minimax.io/docs/api-reference/models/openai/list-models),
[Z.ai Chat Completions](https://docs.z.ai/api-reference/llm/chat-completion),
[Z.ai OpenAPI document](https://docs.z.ai/openapi.json).

## Exact reviewed models and controls

MiniMax seeds `MiniMax-M3`, `MiniMax-M2.7`, `MiniMax-M2.7-highspeed`,
`MiniMax-M2.5` and `MiniMax-M2.5-highspeed`. M3 declares a 1,000,000-token
context and a 524,288-token API output ceiling. M2.x declares a 204,800-token
context; its published output ceiling equals the whole context. The current
budget policy needs a separate output reserve, so the M2.x reserve remains
unknown rather than deriving an impossible input budget. OpenIDE does not
offer effort controls that those exact models ignore. The subscription-only
M3.1 preview is not a general-API seed.

GLM seeds `glm-5.3` and `glm-5.2`, each with a 1,000,000-token context and a
131,072-token output ceiling. The exact documented effort values are exposed
per model. Native image input remains disabled because this transport currently
sends text and tools only. Unknown IDs do not inherit reviewed capabilities.

Sources: [MiniMax request parameters](https://platform.minimax.io/docs/api-reference/text-chat-openai),
[GLM-5.3](https://docs.z.ai/guides/llm/glm-5.3),
[GLM-5.2](https://docs.z.ai/guides/llm/glm-5.2).

## Tool roundtrips and streaming

MiniMax requests `reasoning_split: true`. Z.ai requests
`thinking: {type: "enabled", clear_thinking: false}` and enables `tool_stream`
only for exact models documented to support it. Both consume SSE deltas,
incremental function arguments, usage and the terminating `[DONE]` marker.
MiniMax's HTTP-200 `base_resp.status_code` errors are still failures.

The harness retains returned `reasoning_content` and structured
`reasoning_details` in the assistant's durable `provider_output_items`. The
next tool roundtrip restores those fields without modifying canonical text,
call IDs or tool results. Opaque signatures/data never become visible assistant
text. Existing route reconciliation discards incompatible provider-only state
when the model route changes. Reasoning participates in the aggregate 8 MiB
stream bound and has a maximum of 128 structured blocks.

Sources: [MiniMax interleaved reasoning](https://platform.minimax.io/docs/guides/text-m3-function-call),
[Z.ai thinking mode](https://docs.z.ai/guides/capabilities/thinking-mode),
[Z.ai streamed tools](https://docs.z.ai/guides/capabilities/stream-tool).

## Local contract checks

`provider_chat_contract_tests` exercises two loopback HTTP requests per provider:
Bearer authentication, exact URL/model/options, fragmented Unicode reasoning
and tool arguments, serialized assistant history, tool results, usage, and a
JSON fallback response. Separate checks cover opaque-state presentation,
resource bounds, invalid identities, HTTP-200 errors and streaming-only options.
Catalog tests prove that Z.ai makes no HTTP request and that MiniMax's list
does not fabricate capabilities. These tests use synthetic keys and data; they
do not make paid requests or establish live account availability.
