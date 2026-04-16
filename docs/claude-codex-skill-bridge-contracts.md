# Claude to Codex Skill Bridge Contracts

## MVP Scope Statement

The first implementation slice is intentionally narrow.

Supported:

- Claude Code -> proxy -> Codex request flow
- prompt-heavy skills
- one active skill marker per Anthropic request
- local registry loading from disk
- Codex instruction injection from a mapped `SKILL.md`

Not supported in MVP:

- universal agent support
- automatic mapping of arbitrary Claude runtime hooks
- OpenAI request path skill mapping
- remote install sources
- multi-skill conflict resolution
- tool schema transforms

## MVP Reference Skill

The reference skill for the first end-to-end slice is `code-review`.

Selection rationale:

- it is prompt-centric
- it is easy to express in both Claude and Codex forms
- it is useful enough to validate the full bridge path

## Marker Contract

The Anthropic-side activation marker is a deterministic system sentinel:

```text
skill-bridge:code-review@1.0.0
```

Rules:

- the marker must appear on its own system line
- the proxy reads the first matching marker it finds
- the proxy strips marker lines before sending the final Codex request
- if the marker is missing or unresolved, the request continues without bridge enrichment

The current implementation only scans:

- top-level `system`
- `messages` entries with role `system`

## Claude Plugin Activation Contract

The generated Claude artifact is a plugin-shaped directory:

- `.claude-plugin/plugin.json`
- `commands/<skill-id>.md`

The command body begins with the stable marker line and then includes the canonical Claude prompt body.

The current project-local activation path is:

- install the plugin into `.bridge/agents/claude-code/<skill-id>`
- load it for a Claude session using `claude --plugin-dir <that-path>`

This keeps local activation deterministic without depending on a global Claude plugin install location.

## Canonical Skill Manifest Schema

The canonical skill source is described by a versioned `skill.yaml`.

MVP parser note:

- the initial compiler accepts JSON-compatible YAML in `skill.yaml`
- this keeps the package dependency-free for the first iteration
- full YAML parsing can be added later without changing the manifest contract

Required fields:

```yaml
schema_version: "1"
id: code-review
version: 1.0.0
display_name: Code Review
description: Review repository changes for correctness and risk.
activation:
  marker: skill-bridge:code-review@1.0.0
compatibility:
  claude_code: true
  codex: true
mapping:
  codex_skill: code-review
  merge_mode: prepend
artifacts:
  codex_entry: dist/codex/code-review/SKILL.md
  claude_entry: dist/claude/code-review
```

Optional fields:

```yaml
references:
  - references/review-rubric.md
tool_aliases:
  ReadFile: read_file
  RunTests: test_runner
source_agent: claude
tags:
  - review
  - quality
```

Validation rules:

- `id` must be stable and non-empty
- `version` must be non-empty
- `activation.marker` must be non-empty
- `mapping.codex_skill` must be non-empty
- `mapping.merge_mode` defaults to `prepend`
- `artifacts.codex_entry` must be present for MVP

## Proxy Registry Schema

The compiler emits a JSON registry for the Rust proxy.

Current schema:

```json
{
  "version": "1",
  "skills": [
    {
      "id": "code-review",
      "version": "1.0.0",
      "marker": "skill-bridge:code-review@1.0.0",
      "codex_artifact_path": "code-review/SKILL.md",
      "merge_mode": "prepend",
      "tool_aliases": {},
      "compatibility": {
        "anthropic": true,
        "codex": true
      }
    }
  ]
}
```

Registry rules:

- `marker` is the lookup key
- `codex_artifact_path` may be relative to the registry file
- `reference_bundle_path` may be relative to the registry file
- `merge_mode` currently supports `prepend`, `append`, and `replace`
- `tool_aliases` maps Claude-facing names to upstream Codex-facing names
- `compatibility` is informational in MVP and not yet enforced

## Tool Alias Policy

Tool aliasing is directional at compile time and reversible at runtime.

Rules:

- manifests declare `tool_aliases` as `Claude name -> Codex name`
- Anthropic request translation renames outgoing tool definitions, tool choices, and assistant tool-use blocks
- runtime tool validation keys schemas by the Codex-facing name
- Codex responses are mapped back to the original Claude-facing name before returning to Anthropic

This preserves upstream compatibility without changing the Claude-facing tool contract.

## Reference Loading Policy

The current policy is precompiled reference excerpts.

Rules:

- the manifest may declare `references`
- the compiler reads those files and emits a deterministic `references.json` bundle
- the proxy loads that bundle at request time
- references are appended to the bridge-generated instruction block before user system instructions
- the runtime does not parse arbitrary long reference files ad hoc during request handling

This keeps runtime behavior deterministic and bounds the amount of context injected by the bridge.

## Fallback Contract

If a skill marker is present but cannot be resolved:

- the proxy logs a warning
- marker lines are still stripped from the forwarded request
- the request continues without skill enrichment

If the Codex artifact cannot be loaded:

- the proxy logs a warning
- the request continues without skill enrichment

This keeps the transport path available while the bridge remains additive.
