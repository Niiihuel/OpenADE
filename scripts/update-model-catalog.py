#!/usr/bin/env python3
"""Rebuild the bundled model catalog from models.dev (MIT, public data).

A maintainer command, never part of the app: the app only reads the committed
snapshot. No credentials are sent. Only the fields the model card shows are
kept, for the providers openADE has an adapter for and for text-generating
models with a stated context window.

  python3 scripts/update-model-catalog.py                 # fetch models.dev
  python3 scripts/update-model-catalog.py --api api.json  # from a saved copy
  python3 scripts/update-model-catalog.py --codex-models \\
      references/codex/codex-rs/models-manager/models.json --codex-revision 33a0f766a6

Without --codex-models the Codex CLI table already in the snapshot is kept.
"""

import argparse
import datetime
import json
from pathlib import Path
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "apps/agents-window/assets/model-catalog/models-dev.json"
SOURCE = "https://models.dev/api.json"
# openADE provider id -> models.dev provider id. "codex" (a ChatGPT plan) has
# no models.dev entry of its own; the app borrows only model traits for it.
PROVIDERS = {
    "openai": "openai",
    "anthropic": "anthropic",
    "openrouter": "openrouter",
    "deepseek": "deepseek",
    "minimax": "minimax",
    "zai": "zai",
    "groq": "groq",
    "mistral": "mistral",
    "xai": "xai",
    "opencode": "opencode",
    "nvidia": "nvidia",
}
COST = ("input", "output", "cache_read", "cache_write")
LIMIT = ("context", "input", "output")


def positive(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and value > 0 else None


def model(entry):
    modalities = entry.get("modalities") or {}
    limit = {key: positive((entry.get("limit") or {}).get(key)) for key in LIMIT}
    if "text" not in (modalities.get("output") or []) or not limit["context"]:
        return None
    kept = {}
    for key in ("description", "knowledge", "release_date"):
        if isinstance(entry.get(key), str) and entry[key].strip():
            kept[key] = " ".join(entry[key].split())[:240]
    for key in ("tool_call", "reasoning", "structured_output"):
        if isinstance(entry.get(key), bool):
            kept[key] = entry[key]
    kept["modalities"] = {
        side: [kind for kind in modalities.get(side) or [] if isinstance(kind, str)][:8]
        for side in ("input", "output")
    }
    kept["limit"] = {key: int(value) for key, value in limit.items() if value}
    cost = entry.get("cost") or {}
    # A zero price is a real statement (a free route); a missing one is unknown.
    prices = {
        key: cost[key]
        for key in COST
        if isinstance(cost.get(key), (int, float)) and not isinstance(cost[key], bool) and cost[key] >= 0
    }
    if "input" in prices and "output" in prices:
        kept["cost"] = prices
    return kept


def codex_table(path, revision):
    listed = json.loads(Path(path).read_text())["models"]
    return {
        "source": f"openai/codex codex-rs/models-manager/models.json @ {revision}",
        "models": {
            entry["slug"]: {"context": int(entry["context_window"])}
            for entry in listed
            if entry.get("visibility") == "list" and positive(entry.get("context_window"))
        },
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--api", help="saved copy of api.json instead of fetching")
    parser.add_argument("--codex-models", help="Codex CLI models.json to rebuild its table from")
    parser.add_argument("--codex-revision", default="unknown")
    args = parser.parse_args()
    if args.api:
        api = json.loads(Path(args.api).read_text())
    else:
        with urllib.request.urlopen(SOURCE, timeout=60) as response:
            api = json.load(response)
    previous = json.loads(OUTPUT.read_text()) if OUTPUT.exists() else {}
    providers = {}
    for ours, theirs in PROVIDERS.items():
        models = {id: model(entry) for id, entry in sorted(api[theirs]["models"].items())}
        providers[ours] = {id: kept for id, kept in models.items() if kept}
    snapshot = {
        "source": SOURCE,
        "licence": "MIT License, Copyright (c) 2025 models.dev (https://github.com/anomalyco/models.dev)",
        "date": datetime.date.today().isoformat(),
        "providers": providers,
        "codex_cli": codex_table(args.codex_models, args.codex_revision)
        if args.codex_models
        else previous.get("codex_cli", {"source": "", "models": {}}),
    }
    # One model per line keeps the file compact and its diffs reviewable.
    lines = ["{"]
    for key in ("source", "licence", "date", "codex_cli"):
        lines.append(f"{json.dumps(key)}:{json.dumps(snapshot[key], separators=(',', ':'), ensure_ascii=False)},")
    lines.append('"providers":{')
    blocks = []
    for provider, models in providers.items():
        rows = ",\n".join(
            f"{json.dumps(id)}:{json.dumps(kept, separators=(',', ':'), ensure_ascii=False)}"
            for id, kept in models.items()
        )
        blocks.append(f"{json.dumps(provider)}:{{\n{rows}\n}}")
    lines.append(",\n".join(blocks))
    lines.append("}}")
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text("\n".join(lines) + "\n")
    json.loads(OUTPUT.read_text())
    print(f"{OUTPUT.relative_to(ROOT)}: {sum(map(len, providers.values()))} models, {OUTPUT.stat().st_size} bytes")


if __name__ == "__main__":
    main()
