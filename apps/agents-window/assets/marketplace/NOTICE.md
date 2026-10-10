# Recommended skill catalog

Reviewed 2026-10-07. `starter-sources.json` is the reviewed selection and
`starter-catalog.json` is its generated immutable file manifest. The application
embeds descriptive metadata and hashes only. Browsing does not contact these
repositories; an explicit installation downloads that complete skill directory.
No script, package manager, login or cloud deployment runs during installation.

The catalog contains 36 skills from six official publishers: 318 pinned files,
1,841,398 download bytes across all bundles, and no bundle larger than 238,221
bytes. Categories group design/frontend, quality/testing, data, cloud/deployment,
development workflow and documentation. The three original IDs remain stable.

## Sources and license notices

- [Anthropic · anthropics/skills](https://github.com/anthropics/skills/tree/683bc88e56f3e09ba94f7055977f3d3aa499f202):
  commit `683bc88e56f3e09ba94f7055977f3d3aa499f202`, Apache-2.0, 6 skills.
  Every selected skill includes its original `LICENSE.txt`; the repository
  `THIRD_PARTY_NOTICES.md` is preserved at `_upstream/THIRD_PARTY_NOTICES.md`.
- [Vercel · vercel-labs/agent-skills](https://github.com/vercel-labs/agent-skills/tree/063bee94c3f4df8453406c830b0a7df0f2860278):
  commit `063bee94c3f4df8453406c830b0a7df0f2860278`, MIT, 4 skills.
  The original `SKILL.md` and root README declare MIT. The README is
  preserved at `_upstream/README.md`. Upstream supplies no separate license
  file or copyright statement at this commit; OpenIDE does not invent one.
- [Supabase · supabase/agent-skills](https://github.com/supabase/agent-skills/tree/c9be0e931b7930f7d02126d04774d904c381e7d7):
  commit `c9be0e931b7930f7d02126d04774d904c381e7d7`, MIT, 1 skills.
  The full repository `LICENSE`, including its original notice, is preserved
  at `_upstream/LICENSE` in every installed bundle.
- [Sentry · getsentry/skills](https://github.com/getsentry/skills/tree/d18b7aa8ba878354e5c348310230e652f7690f9c):
  commit `d18b7aa8ba878354e5c348310230e652f7690f9c`, Apache-2.0, 11 skills.
  The full repository `LICENSE`, including its original notice, is preserved
  at `_upstream/LICENSE` in every installed bundle.
- [Microsoft · microsoft/skills](https://github.com/microsoft/skills/tree/354361d83247c76a1c21e802e0d4887c4d8323a3):
  commit `354361d83247c76a1c21e802e0d4887c4d8323a3`, MIT, 12 skills.
  The full repository `LICENSE`, including its original notice, is preserved
  at `_upstream/LICENSE` in every installed bundle.
- [Cloudflare · cloudflare/skills](https://github.com/cloudflare/skills/tree/41e0d19858946d18af9ee2c2feebbe2e11d829ff):
  commit `41e0d19858946d18af9ee2c2feebbe2e11d829ff`, Apache-2.0, 2 skills.
  The full repository `LICENSE`, including its original notice, is preserved
  at `_upstream/LICENSE` in every installed bundle.

The [Agent Skills format](https://agentskills.io/specification) makes the
instruction files readable by multiple agents; it does not make every workflow
independent of tools. Compatibility details are shown for each entry:

- React/Next.js, React Native/Expo and TypeScript examples require their own
  project dependencies. React View Transition support depends on runtime and
  framework versions, including experimental APIs described upstream.
- Anthropic's brand guide is specifically for its brand. The algorithmic-art
  template needs a browser and p5.js from a CDN. Webapp testing needs Python,
  Playwright and browsers. MCP development needs its selected language and SDK.
- Sentry conventions are identified as such. Git workflows need Git; find-bugs
  and PR publishing use authenticated GitHub CLI. `document-api-endpoint` is
  specifically for Sentry's Django/DRF/drf-spectacular codebase.
- Microsoft entries are SDK/code-generation guidance, not the separate Azure
  plugin that requires MCP servers. Running Azure examples needs the indicated
  SDKs, runtime and authorized cloud identity/resources.
- Cloudflare entries need the project's Wrangler/runtime for local commands and
  an authorized account or token for remote operations. No account is connected
  as part of installation.

All directory contents, including scripts, templates, AGENTS.md and references,
are retained byte-for-byte. Scripts are downloaded as regular data files, not
executed or implicitly made trusted. Tool names from other clients remain
upstream text and do not register tools or grant permissions in OpenIDE.
Some aggregate upstream AGENTS.md documents contain links written for their
source reference directory; their full original reference files are included
and can be resolved from the installed skill directory. No instruction text is
rewritten. Proprietary Anthropic document skills and MCP-dependent Azure plugin
bundles were not selected.

## Reproducing an update

After reviewing each new revision and license, edit `starter-sources.json` and
run `rtk python3 scripts/update-marketplace-catalog.py`. Use `--check` to verify
that the generated manifest is unchanged, optionally with `--cache PATH` to
reuse downloaded public files. The generator uses complete Git trees, rejects
mutable refs, symlinks/submodules, redirects and oversized bundles, verifies Git
blob identities and records exact SHA-256 and byte sizes. It never executes
upstream code. Review metadata, new files and license notices before shipping.

The installer verifies every file against this manifest. Each bundle records
repository, directory, revision and a digest of the exact file membership in
`.openide-source.json`. App updates do not automatically replace installed
skills; updating a selected installed bundle is an explicit operation. Skills
remain subject to the native permission policy. Enabled skills initially add
only discovery metadata to the agent context; full instructions and references
are read on demand.
