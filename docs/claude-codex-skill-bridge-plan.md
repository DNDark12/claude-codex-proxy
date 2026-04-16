# Claude to Codex Skill Bridge Plan

## Status

Draft design note for introducing a skill distribution and runtime bridge that maps Claude-oriented skills to Codex-native skills in this repository.

## Problem Statement

The current proxy translates Anthropic and OpenAI requests into Codex Responses API calls, but it does not have a first-class concept of "skills". That leaves a gap:

- Claude users can activate skills, plugins, slash commands, and agent-specific workflows.
- Codex has its own native skill format and instruction model.
- The proxy currently bridges transport and tool calls, not agent capability packaging.

The goal is to let a team define a skill once, install it with a simple command, and have:

- Claude receive a native activation surface.
- Codex receive a native skill bundle or equivalent instruction payload.
- The proxy map Claude skill activation to the corresponding Codex skill at request time.

## Target Outcome

After the system is implemented, the intended user flow is:

1. A team publishes or stores a canonical skill source.
2. A user runs `npx <bridge-cli> add ...`.
3. The installer adds the skill to supported agents, starting with Claude Code and Codex.
4. Claude activates the skill through a native surface such as a plugin command, slash command, or declared skill.
5. The proxy detects a stable skill marker in the Anthropic request.
6. The proxy resolves that marker to a Codex-native skill definition.
7. The outgoing Codex request contains the mapped instructions, references, and tool aliases needed for equivalent behavior.

## External References

This design is informed by two existing patterns:

- `vercel-labs/skills`
  - Uses a CLI install model such as `npx skills add ...`
  - Distributes skills to multiple agents
  - Supports project/global scope and symlink/copy installation models
- `openai/codex-plugin-cc`
  - Uses a Claude-native plugin package to expose Codex functionality
  - Treats Claude UX and Codex runtime as separate layers
  - Avoids forcing all semantics into a transport-only bridge

## Current Repository Constraints

The current Rust proxy is a good runtime bridge base, but not yet a skill platform.

Relevant files:

- `src/routes/mod.rs`
  - Handles Anthropic and OpenAI requests
  - Best entry point for request-time skill resolution
- `src/translation/anthropic_to_codex.rs`
  - Builds the final `instructions` string for Codex
  - Good place for final instruction composition, but not for registry ownership
- `src/translation/tool_format.rs`
  - Already normalizes tool names and tool choice semantics
  - Natural location for future skill-specific tool alias mapping
- `src/main.rs`
  - Good place to initialize a shared skill registry

## Design Principles

1. Keep agent-native UX agent-native.
Claude should receive Claude-native plugins, commands, and activation flows instead of relying on prompt guessing.

2. Keep Codex-native runtime Codex-native.
Mapped skills should become Codex skill bundles or Codex-compatible instruction payloads, not just free-form text when a stronger mapping exists.

3. Do not infer active skills from arbitrary prompts.
Skill activation must be attached through a stable machine-readable marker.

4. Keep the proxy focused on runtime mapping.
Installation, packaging, generation, and publishing should live outside the Rust proxy.

5. Start narrow.
The first supported targets should be:
- Claude Code
- Codex

## Non-Goals for MVP

- Supporting every current AI agent on day one
- Perfect one-to-one support for every arbitrary Claude skill
- Full emulation of Claude-only runtime behavior inside the proxy
- Automatic conversion of complex agent hooks without explicit adapter logic

## Proposed System Overview

The system should be split into five layers.

### 1. Canonical Skill Source

Each skill should live in a canonical source folder with a shared manifest and source materials.

Example layout:

```text
skills/
  example-skill/
    skill.yaml
    source/
      claude.md
    references/
      usage.md
      policies.md
    scripts/
      helper.sh
    adapters/
      claude/
      codex/
```

This is the source of truth. Agent-specific output is generated from this source.

### 2. Skill Compiler

A compiler package should transform the canonical source into agent-specific artifacts.

Outputs:

- Claude plugin or Claude skill bundle
- Codex skill folder with `SKILL.md`
- Registry metadata used by the Rust proxy

The compiler should support deterministic generation so that rebuilds are stable and reviewable.

### 3. Skills Installer CLI

A Node-based CLI should provide the user experience inspired by `vercel-labs/skills`.

Proposed commands:

- `npx <bridge-cli> add <source>`
- `npx <bridge-cli> list`
- `npx <bridge-cli> update`
- `npx <bridge-cli> remove`
- `npx <bridge-cli> doctor`

Required install features:

- Project scope and global scope
- Symlink mode by default
- Copy mode as fallback
- Agent selection flags
- Skill selection flags

### 4. Claude Activation Layer

A Claude-native plugin package should expose stable activation points.

This layer can include:

- plugin metadata
- slash commands
- prompts
- hooks
- subagents
- Claude-local skill files

Its main responsibility is not heavy logic. Its main responsibility is to inject a stable skill marker into Claude-originated requests.

### 5. Rust Proxy Runtime Bridge

The current proxy becomes the request-time mapping layer.

Responsibilities:

- read incoming Claude request
- detect skill marker
- resolve marker against local registry
- load mapped Codex skill materials
- merge instructions and references
- rename or normalize tools if needed
- forward the resulting request to Codex

## Why a Claude Plugin Layer Is Necessary

The `codex-plugin-cc` pattern is a strong indication that Claude should be integrated through Claude-native packaging, not just transport conversion.

Reasons:

- Claude needs a native activation surface.
- Plugins and commands provide predictable entry points.
- The plugin can carry skill identity explicitly.
- The proxy becomes simpler because it receives a stable signal instead of trying to infer behavior from prompt text.

Without this layer, the proxy would need fragile heuristics such as:

- prompt substring matching
- guessing from system messages
- guessing from tool names

That approach will not be robust enough for production use.

## Canonical Manifest Proposal

Each skill should have a machine-readable manifest, for example `skill.yaml`.

Suggested fields:

```yaml
id: code-review
version: 1.0.0
display_name: Code Review
description: Review repository changes for correctness and risk.
source_agent: claude
activation:
  marker: skill-bridge:code-review@1.0.0
  aliases:
    - code-review
compatibility:
  claude_code: true
  codex: true
mapping:
  codex_skill: code-review
  tool_aliases:
    ReadFile: read_file
    RunTests: test_runner
  instruction_mode: prepend
references:
  - references/usage.md
  - references/policies.md
artifacts:
  claude_plugin_path: dist/claude/code-review
  codex_skill_path: dist/codex/code-review
```

Required properties:

- `id`
- `version`
- `activation.marker`
- `mapping.codex_skill`

## Skill Categories

The bridge should explicitly classify skills to avoid overpromising support.

### Category A: Prompt-Only Skills

These mostly provide instructions, heuristics, formatting rules, and references.

Support target:
- Full support in MVP

### Category B: Prompt Plus References or Scripts

These require references or deterministic helper scripts.

Support target:
- Full support in MVP if scripts are local and compatible with the target environment

### Category C: Agent Runtime Skills

These depend on hooks, local UI flows, proprietary runtime events, or special Claude behavior.

Support target:
- Partial support only
- Must declare adapter-specific logic
- Unsupported by default until an explicit mapping exists

## Installation Model

The installer should maintain one canonical cache and fan out artifacts to agent-specific directories.

### Scope

- Project scope
  - committed with the project
  - suitable for shared team workflows
- Global scope
  - available across repositories on one machine

### Method

- Symlink
  - default
  - easiest to update
  - single source of truth
- Copy
  - fallback when symlink is not viable

### Proposed Local Layout

```text
.bridge/
  registry.json
  store/
    code-review/
      1.0.0/
        canonical/
        dist/
          claude/
          codex/
```

For project installs, the agent-facing install paths should point to `.bridge/store/...`.

## Runtime Mapping Flow

### Anthropic Request Path

1. Claude sends a request to the proxy.
2. The request includes a stable marker, preferably in the system instruction or a dedicated metadata block when available.
3. `src/routes/mod.rs` parses the Anthropic request.
4. The proxy extracts the marker before translation.
5. The proxy looks up the marker in a loaded registry.
6. The proxy loads the mapped Codex skill data.
7. The proxy augments the request context.
8. The translator builds the final Codex request.
9. The proxy sends the request upstream.

### Mapping Actions

The resolver may perform any combination of:

- prepend mapped Codex instructions
- append selected reference excerpts
- inject a Codex skill identifier into instructions
- translate tool names
- adjust tool choice policy
- strip Claude-only marker text before upstream send

## Injection Strategy

The instruction merge strategy should be explicit and deterministic.

Proposed order:

1. bridge-generated Codex skill instruction
2. user or project system instruction
3. Claude request system text
4. request messages

Recommended merge modes:

- `prepend`
- `append`
- `replace`

For MVP, use `prepend` only.

## Tool Mapping Strategy

Not all skills are prompt-only. Some may assume tool names or schemas.

The bridge should support a tool alias table:

- Claude tool name
- Codex tool name
- optional schema transformer

The existing tool normalization logic in `src/translation/tool_format.rs` should be extended rather than bypassed.

Example use cases:

- Claude skill expects `text_editor`
- Codex runtime exposes `edit_file`
- Bridge maps the name and normalizes arguments

## Required Changes in This Repository

### Rust Side

Add a new `skills` module.

Suggested layout:

```text
src/skills/
  mod.rs
  registry.rs
  resolver.rs
  manifest.rs
  loader.rs
```

Responsibilities:

- parse generated registry
- resolve markers
- load Codex artifact metadata
- expose a simple API to routes

### Route Layer

In `src/routes/mod.rs`:

- add `SkillRegistry` to `AppState`
- resolve active skill during Anthropic request handling
- build an enriched request context before translation

### Translation Layer

In `src/translation/anthropic_to_codex.rs`:

- keep translation mostly pure
- allow instruction composition from an already-resolved bridge context

In `src/translation/tool_format.rs`:

- add optional tool alias mapping hooks

### Main Initialization

In `src/main.rs`:

- load registry path from env or CLI
- initialize `SkillRegistry`
- pass it into `build_routes`

## Suggested Installer and Compiler Packages

The skill packaging logic should not be implemented in Rust first.

Suggested workspace additions:

```text
packages/
  skills-cli/
  skill-compiler/
```

### `skills-cli`

Responsibilities:

- add, remove, list, update
- local registry management
- symlink or copy install
- source fetch and install workflow

### `skill-compiler`

Responsibilities:

- convert canonical source to Claude and Codex artifacts
- validate manifest
- generate proxy registry entries

## CLI UX Proposal

Examples:

```bash
npx skill-bridge add owner/repo
npx skill-bridge add owner/repo --agent claude-code --agent codex
npx skill-bridge add owner/repo --skill code-review -g
npx skill-bridge update
npx skill-bridge list
npx skill-bridge doctor
```

Important behavior:

- first-class support for project and global modes
- first-class support for selecting agents
- ability to list available skills before install

## Repository Milestones

### Milestone 0: Design and Contracts

Deliverables:

- manifest schema draft
- registry schema draft
- marker strategy
- folder structure decision

Exit criteria:

- no ambiguity about how a skill is identified
- no ambiguity about where generated artifacts live

### Milestone 1: Prompt-Only Skill MVP

Deliverables:

- canonical manifest
- Codex `SKILL.md` generator
- Claude plugin artifact generator
- Rust registry loader
- Anthropic request marker extraction
- instruction injection into Codex request

Exit criteria:

- one simple Claude-originated skill maps successfully into Codex
- no prompt heuristics are required

### Milestone 2: Installer CLI

Deliverables:

- `add`, `list`, `remove`, `update`
- project and global install
- symlink and copy modes
- local registry persistence

Exit criteria:

- a user can install a skill with one command
- installed skill works in both Claude Code and Codex

### Milestone 3: Tool Aliases and References

Deliverables:

- tool alias mapping
- reference loading policy
- script pass-through rules

Exit criteria:

- a non-trivial skill with tools and references maps correctly

### Milestone 4: Agent Expansion

Deliverables:

- additional adapters for Cursor or other agents
- compatibility matrix

Exit criteria:

- new agents can be added without changing the Rust bridge core

## Testing Strategy

### Unit Tests

- manifest parsing
- registry parsing
- marker extraction
- instruction merge behavior
- tool alias resolution

### Fixture Tests

Use golden fixtures for:

- canonical skill source
- generated Claude artifact
- generated Codex `SKILL.md`
- generated registry entry

### Integration Tests

Add end-to-end tests that:

1. send an Anthropic request with a skill marker
2. resolve the registry
3. generate a Codex request
4. assert the expected instruction payload and tool mapping

### Regression Tests

Protect against:

- missing marker fallback behavior
- unsupported skill category behavior
- tool name collisions
- duplicate skill ids across versions

## Logging and Diagnostics

Add structured logs for:

- detected skill marker
- resolved skill id and version
- unsupported skill category
- missing registry entry
- applied tool aliases

Add a future `doctor` command to validate:

- registry integrity
- install targets
- broken symlinks
- missing Codex artifacts

## Risk Register

### Risk: Overpromising universal Claude skill compatibility

Reality:
- many Claude skills are not portable without explicit adapter logic

Mitigation:
- classify support categories
- require explicit compatibility metadata

### Risk: Proxy becomes overloaded with packaging concerns

Reality:
- installer and compiler logic do not belong in the Rust runtime bridge

Mitigation:
- keep packaging in Node packages
- keep Rust focused on runtime resolution

### Risk: Prompt-based inference becomes fragile

Reality:
- prompt guessing will drift and break

Mitigation:
- require machine-readable activation markers

### Risk: Tool mismatches break execution

Reality:
- even similar agents often expose different tool names and schemas

Mitigation:
- add explicit alias tables
- add schema transformers only where required

## Open Questions

1. Where should the canonical skill source live:
   - inside this repository
   - in a separate skills repository
   - both, with import support

2. What is the best marker transport:
   - system text marker
   - hidden metadata block
   - tool declaration
   - plugin-managed command prefix

3. Should Codex receive generated `SKILL.md` on disk only, or can some skills be inlined as runtime instructions

4. How should references be loaded:
   - always inline selected excerpts
   - lazy load by resolver
   - hybrid based on manifest rules

5. How much of Claude plugin generation should be templated versus fully compiled

## Recommended MVP Scope

To keep the first iteration feasible:

- support one prompt-heavy skill
- support Claude Code and Codex only
- use a single marker mechanism
- use `prepend` instruction mode only
- defer complex hooks and runtime-specific Claude behaviors

## Recommended Next Actions

1. Finalize `skill.yaml` and registry schemas.
2. Add `src/skills` to this Rust project.
3. Introduce a registry path configuration in `main.rs`.
4. Update Anthropic request handling to resolve skill markers.
5. Build a minimal compiler that generates:
   - Claude plugin artifact
   - Codex `SKILL.md`
   - proxy registry entry
6. Add a minimal `skills-cli` with `add` and `list`.
7. Prove the flow with one reference skill end to end.

## Summary

The recommended architecture is:

- canonical skill source
- compiler that emits Claude and Codex artifacts
- installer CLI inspired by `vercel-labs/skills`
- Claude-native plugin for stable activation
- Rust proxy runtime bridge for marker resolution and Codex mapping

This keeps installation, activation, and runtime concerns separated and gives the project a path from a transport proxy into a real cross-agent skill bridge.
