# Bundled model catalog

`models-dev.json` is a reduced snapshot of the public catalog at
<https://models.dev/api.json> (source: <https://github.com/anomalyco/models.dev>),
MIT License, Copyright (c) 2025 models.dev. Its `date` field is the day it was
taken.

Only the providers openADE has an adapter for are kept, only text-generating
models with a stated context window, and only the fields the model card shows:
description, tool calling, reasoning, structured output, modalities, token
limits, reference prices (USD per million tokens), knowledge cutoff and release
date. The file holds no account data and the app never fetches it at run time.

`codex_cli` is the context window the Codex CLI ships for each model of a
ChatGPT plan, from `codex-rs/models-manager/models.json` of
<https://github.com/openai/codex> (Apache-2.0) at the revision the field names.

Regenerate both with `python3 scripts/update-model-catalog.py` (see its header).
