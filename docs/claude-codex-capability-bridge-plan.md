# Claude to Codex Capability Bridge Plan

## Status

Draft design note for mapping Claude Code default capabilities to Codex-native runtime behavior in this repository.

This document is intentionally separate from `docs/claude-codex-skill-bridge-plan.md`.

- `skill bridge plan` covers custom skills that we control through manifests and registries
- `capability bridge plan` covers provider-native behavior that exists even without custom skill installation

## Problem Statement

The current bridge can map custom Claude-oriented skills to Codex when those skills are installed and resolved through a stable marker.

That is not enough for the higher-priority goal:

- Claude Code has built-in tools and built-in command surfaces
- Codex has its own runtime model, tool semantics, approval model, and workflow surface
- users expect Claude-native behavior to remain usable when requests are bridged to Codex

The missing layer is a provider capability bridge, not another installer feature.

## Goal

Make the proxy reliably translate the most important default Claude Code capabilities into equivalent or explicitly downgraded Codex behavior.

The bridge should answer, for every supported Claude capability:

- can this map natively to Codex
- can this be emulated with instructions and policy
- can this only be partially mapped
- is this unsupported and needs an explicit fallback

## Verified External References

Verified on April 16, 2026:

- Anthropic Claude Code settings and built-in tools:
  - https://docs.anthropic.com/en/docs/claude-code/settings
- Anthropic Claude Code slash commands:
  - https://docs.anthropic.com/en/docs/claude-code/slash-commands
- OpenAI Codex getting started and approval workflow overview:
  - https://help.openai.com/en/articles/11096431-openai-codex-ci-getting-started

## Known Claude Capability Surface

Based on the Anthropic documentation above, Claude Code currently exposes built-in tools such as:

- `Bash`
- `Edit`
- `Glob`
- `Grep`
- `LS`
- `MultiEdit`
- `NotebookEdit`
- `NotebookRead`
- `Read`
- `Task`
- `TodoWrite`
- `WebFetch`
- `WebSearch`
- `Write`

It also exposes built-in slash-command surfaces such as:

- `/agents`
- `/doctor`
- `/init`
- `/mcp`
- `/memory`
- `/review`

Not all of these are "skills". Some are tools, some are workflows, some are configuration or UX entry points.

## Why the Existing Skill Bridge Is Not Sufficient

The implemented skill bridge assumes:

- we control the source manifest
- we control the activation marker
- we control the mapping contract

Default Claude capabilities do not satisfy those assumptions.

Examples:

- `Read` and `Edit` are runtime tools, not installed skills
- `/review` is a built-in command surface, not a manifest-driven plugin
- `/memory` and approval-related behaviors carry state and policy, not just prompt text

This means the next subsystem must be built around capability semantics, not package installation.

## Design Principles

1. Treat provider-native behavior as capabilities, not custom skills.
2. Separate UX surface from executable runtime behavior.
3. Prefer native runtime mapping over prompt emulation.
4. Make every downgrade explicit.
5. Do not silently claim parity where semantics differ.
6. Keep compatibility decisions versioned and testable.

## Scope

In scope:

- Claude default tools
- Claude default slash-command behavior where it changes runtime intent
- mapping policy for approval and execution semantics
- explicit unsupported and fallback behavior
- conformance tests for each supported capability

Out of scope for the first implementation:

- custom skill distribution
- global install
- remote source install
- universal support for every Claude-only hook or plugin behavior
- perfect bidirectional parity for every Codex-native feature

## Capability Taxonomy

The bridge should classify capabilities into five families.

### 1. File and Code Tools

Examples:

- `Read`
- `Write`
- `Edit`
- `MultiEdit`
- `Glob`
- `Grep`
- `LS`

### 2. Execution and Planning Tools

Examples:

- `Bash`
- `Task`
- `TodoWrite`

### 3. Web and External Context Tools

Examples:

- `WebFetch`
- `WebSearch`

### 4. Notebook and Specialized Document Tools

Examples:

- `NotebookRead`
- `NotebookEdit`

### 5. Workflow and UX Surfaces

Examples:

- `/review`
- `/memory`
- `/doctor`
- `/init`
- `/agents`
- `/mcp`

## Canonical Capability Model

Add a provider-neutral capability model so mappings do not become ad hoc string substitutions.

Each capability record should define:

- `id`
- `family`
- `provider_surface`
- `input_shape`
- `output_shape`
- `side_effect_level`
- `state_scope`
- `approval_requirements`
- `mapping_strategy`
- `fallback_strategy`
- `notes`

Suggested `mapping_strategy` values:

- `native_map`
- `prompt_emulate`
- `partial_map`
- `unsupported`

Suggested `state_scope` values:

- `stateless`
- `request`
- `session`
- `workspace`

## Compatibility Matrix

The first concrete deliverable should be a compatibility matrix that maps every default Claude capability to a Codex outcome.

Example shape:

| Claude capability | Family | Codex target | Strategy | Notes |
| --- | --- | --- | --- | --- |
| `Read` | file | file read tool/runtime | `native_map` | direct parity expected |
| `Write` | file | file write tool/runtime | `native_map` | preserve overwrite semantics |
| `Edit` | file | edit tool/runtime | `native_map` | confirm patch granularity |
| `MultiEdit` | file | edit batching or repeated edit flow | `partial_map` | atomicity may differ |
| `Bash` | execution | shell execution | `native_map` | align approval policy |
| `Task` | planning | delegated plan/subagent workflow | `partial_map` | semantics likely differ |
| `TodoWrite` | planning | plan/task tracking | `prompt_emulate` | may need internal task list |
| `WebFetch` | web | fetch/web tool | `native_map` or `partial_map` | depends on runtime exposure |
| `WebSearch` | web | search/web tool | `partial_map` | align filtering behavior |
| `NotebookRead` | notebook | notebook/document read path | `partial_map` | verify format support |
| `NotebookEdit` | notebook | notebook edit path | `partial_map` or `unsupported` | verify cell-level parity |
| `/review` | workflow | review instruction + tool policy | `prompt_emulate` | high-value early workflow |
| `/memory` | workflow | memory/config equivalent | `partial_map` | state model likely differs |
| `/doctor` | workflow | diagnostics command | `unsupported` or `partial_map` | local UX-specific |
| `/agents` | workflow | subagent listing/config | `unsupported` or `partial_map` | surface mismatch |
| `/init` | workflow | repo bootstrap guidance | `prompt_emulate` | may be instruction template |
| `/mcp` | workflow | MCP config/runtime surface | `partial_map` | depends on host integration |

This matrix should be treated as the contract for implementation order.

## Implementation Strategy

Build the bridge in six phases.

### Phase A: Capability Inventory

Deliverables:

- `docs/claude-capabilities.md`
- `docs/codex-capabilities.md`
- initial `docs/claude-codex-compatibility-matrix.md`

Tasks:

- list every Claude default capability currently relevant to the proxy
- list available Codex runtime surfaces used by this repo
- annotate semantic mismatches, state requirements, and approval concerns

Exit criteria:

- every capability is classified
- every capability has an initial strategy
- ambiguous cases are explicitly marked

### Phase B: Canonical Capability Layer

Deliverables:

- new Rust module, likely `src/capabilities/`
- typed capability definitions
- resolver helpers for capability-based mapping

Tasks:

- define provider-neutral capability types
- add capability resolution path in request translation
- keep this layer separate from custom skill registry logic

Exit criteria:

- translation code can branch on canonical capability identifiers
- capability resolution is unit tested

### Phase C: Tool Parity Bridge

Priority targets:

- `Read`
- `Write`
- `Edit`
- `Glob`
- `Grep`
- `LS`
- `Bash`

Deliverables:

- request-side tool mapping
- response-side reverse mapping where needed
- approval and schema normalization rules

Likely code impact:

- [src/translation/tool_format.rs](/Users/DNDark/Workspaces/codex-openai-proxy/src/translation/tool_format.rs)
- [src/translation/tool_runtime.rs](/Users/DNDark/Workspaces/codex-openai-proxy/src/translation/tool_runtime.rs)
- [src/routes/mod.rs](/Users/DNDark/Workspaces/codex-openai-proxy/src/routes/mod.rs)

Exit criteria:

- direct tool parity works for the highest-volume code and shell operations
- mismatches are surfaced through warnings or explicit fallback behavior

### Phase D: Workflow Emulation

Priority targets:

- `/review`
- `TodoWrite`
- `Task`
- `/init`

Deliverables:

- instruction templates or workflow adapters
- explicit downgrade notes where Codex semantics differ
- tests proving predictable behavior

Likely code impact:

- [src/translation/anthropic_to_codex.rs](/Users/DNDark/Workspaces/codex-openai-proxy/src/translation/anthropic_to_codex.rs)
- new prompt or capability adapter files under `src/capabilities/`

Exit criteria:

- review workflow is usable and deterministic
- plan and task-list behavior is documented as native or emulated

### Phase E: Web and Notebook Coverage

Priority targets:

- `WebFetch`
- `WebSearch`
- `NotebookRead`
- `NotebookEdit`

Deliverables:

- parity rules for web behavior
- notebook support contract
- capability-specific fallbacks

Exit criteria:

- supported cases are explicit
- unsupported notebook semantics fail clearly instead of degrading silently

### Phase F: State, Config, and Unsupported Surface Hardening

Priority targets:

- `/memory`
- `/doctor`
- `/agents`
- `/mcp`

Deliverables:

- stateful behavior policy
- unsupported capability policy
- observability and diagnostics for downgrade decisions

Exit criteria:

- operator can see which capability was requested and how it was handled
- unsupported capability handling is explicit and test-covered

## Fallback Policy

Every capability must choose one fallback mode.

Allowed modes:

- `hard_error`
- `soft_warning_and_continue`
- `downgrade_to_instruction`
- `drop_with_observability`

Rules:

- runtime tools should not silently degrade to vague prompt text if side effects matter
- workflow surfaces may degrade to instructions if behavior remains predictable
- stateful features must declare whether state is preserved, approximated, or lost

## Observability Requirements

Add capability-level logging and diagnostics.

Each bridged request should make it possible to inspect:

- requested Claude capability
- chosen mapping strategy
- whether downgrade occurred
- whether reverse mapping was applied
- reason for unsupported behavior if applicable

This is necessary for debugging parity regressions.

## Testing Strategy

Add conformance tests per capability family.

Test layers:

- unit tests for capability classification
- request translation tests for tool and workflow mapping
- response translation tests for reverse aliasing
- golden fixtures for end-to-end Anthropic request to Codex request transformation

Priority fixture coverage:

- `Read`
- `Write`
- `Edit`
- `Bash`
- `Grep`
- `Glob`
- `LS`
- `/review`

## Recommended MVP

The first capability-bridge release should support:

- `Read`
- `Write`
- `Edit`
- `Bash`
- `Glob`
- `Grep`
- `LS`
- `/review`

Reason:

- this is the highest-value working set for code navigation, code change, shell execution, and review workflow
- this set minimizes stateful edge cases while covering the main developer loop

## Impact on Current Repository

This plan shifts priority away from installer work and toward runtime compatibility.

Expected impact:

- the existing custom skill bridge remains useful for team-defined skills
- `packages/skills-cli` and `packages/skill-compiler` become secondary for now
- most new work moves into translation, routing, and a new capability subsystem

Recommended new code areas:

- `src/capabilities/mod.rs`
- `src/capabilities/model.rs`
- `src/capabilities/matrix.rs`
- `src/capabilities/resolver.rs`
- `tests/fixtures/capabilities/`

## Risks

Primary risks:

- over-claiming parity where provider semantics differ
- mixing custom skill logic with default capability logic
- relying on prompt emulation when runtime mapping is required
- under-testing stateful or approval-sensitive behaviors

Mitigations:

- keep the compatibility matrix explicit and versioned
- enforce per-capability fallback rules
- log downgrade decisions
- gate support claims behind conformance tests

## Immediate Next Actions

1. Create `docs/claude-codex-compatibility-matrix.md` with one row per default Claude capability.
2. Add `src/capabilities/` and define the canonical capability model.
3. Implement Phase C for `Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, and `LS`.
4. Add `/review` emulation in the Anthropic-to-Codex translation path.
5. Add conformance fixtures and regression tests before expanding to stateful surfaces.
