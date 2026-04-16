# Claude to Codex Skill Bridge Tasks

## Purpose

This document turns the design plan in `docs/claude-codex-skill-bridge-plan.md` into an execution backlog with concrete tasks, dependencies, deliverables, and acceptance criteria.

The intent is to make implementation order explicit and reduce ambiguity before coding starts.

## Delivery Strategy

Build the system in four implementation waves:

1. Contracts and runtime hook points
2. End-to-end prompt-only skill MVP
3. Installer and artifact generation
4. Tool mapping, references, and agent expansion

The first release should prove one narrow but stable flow:

- one canonical skill
- one Claude activation path
- one Codex mapping path
- one working Anthropic request through the proxy

## Prioritization Rules

- Prefer explicit contracts over flexible heuristics.
- Prefer one end-to-end slice over multiple incomplete subsystems.
- Prefer generated artifacts over handwritten per-agent copies.
- Prefer deterministic behavior over convenience magic.
- Defer multi-agent fan-out until Claude Code and Codex are stable.

## Workstreams

- WS1: Contracts and schemas
- WS2: Rust runtime bridge
- WS3: Skill compiler
- WS4: Installer CLI
- WS5: Claude plugin adapter
- WS6: Tests and fixtures
- WS7: Docs and operations

## Phase 0: Alignment and Contracts

### Task P0-01: Freeze MVP scope

Status:
- done

Workstream:
- WS1

Description:
- Define the exact MVP boundaries so implementation does not drift into universal agent support too early.

Deliverables:
- a short written scope statement
- selected reference skill for MVP
- explicit list of unsupported features

Acceptance criteria:
- the team agrees MVP supports Claude Code -> proxy -> Codex only
- the team agrees MVP supports prompt-heavy skills only
- unsupported categories are named explicitly

Dependencies:
- none

### Task P0-02: Finalize canonical skill manifest schema

Status:
- done

Workstream:
- WS1

Description:
- Turn the draft `skill.yaml` idea into a stable versioned schema used by the compiler, installer, and proxy registry generator.

Deliverables:
- manifest field list
- required vs optional field rules
- schema examples for valid and invalid skills

Acceptance criteria:
- schema covers identity, versioning, compatibility, activation marker, mapping, and artifacts
- schema is strict enough to reject incomplete skills
- schema is simple enough to hand-author for MVP

Dependencies:
- P0-01

### Task P0-03: Finalize proxy registry schema

Status:
- done

Workstream:
- WS1

Description:
- Define the generated registry format consumed by Rust runtime resolution.

Deliverables:
- registry schema
- sample registry JSON
- versioning strategy for schema changes

Acceptance criteria:
- registry can map marker -> skill id -> Codex artifact path
- registry can carry tool alias data
- registry can carry compatibility flags and merge mode

Dependencies:
- P0-02

### Task P0-04: Choose marker transport strategy

Status:
- done

Workstream:
- WS1

Description:
- Decide how the Claude-side activation marker will be carried into requests.

Options to evaluate:
- system text prefix
- hidden metadata block if Claude plugin model supports it
- command-injected sentinel text

Recommendation:
- start with a deterministic system text sentinel under plugin control

Deliverables:
- marker format
- placement rules
- stripping rules before upstream send if needed

Acceptance criteria:
- proxy can extract the marker without prompt guessing
- marker is stable across runs
- marker does not depend on arbitrary user wording

Dependencies:
- P0-01

### Task P0-05: Select the MVP reference skill

Status:
- done

Workstream:
- WS1

Description:
- Pick one prompt-centric skill to prove the whole bridge.

Selection criteria:
- mostly instructions and references
- minimal runtime coupling
- valuable enough to justify the plumbing

Recommended candidates:
- code review
- architecture review
- refactor guidance

Acceptance criteria:
- one skill is chosen and documented
- the skill can be expressed cleanly in both Claude and Codex forms

Dependencies:
- P0-01

## Phase 1: Rust Runtime Bridge Skeleton

### Task P1-01: Add `skills` module to Rust project

Status:
- done

Workstream:
- WS2

Description:
- Introduce a dedicated module for skill registry loading and runtime resolution.

Target files:
- `src/skills/mod.rs`
- `src/skills/manifest.rs`
- `src/skills/registry.rs`
- `src/skills/resolver.rs`
- `src/skills/loader.rs`

Deliverables:
- compileable module structure
- public API for registry load and marker resolution

Acceptance criteria:
- project builds with the new module
- module boundaries are clear
- no route logic is hardcoded into the module internals

Dependencies:
- P0-03

### Task P1-02: Add registry path configuration

Status:
- done

Workstream:
- WS2

Description:
- Allow the proxy to load a generated registry from CLI or environment.

Target files:
- `src/main.rs`

Suggested config:
- `--skills-registry-path`
- `PROXY_SKILLS_REGISTRY_PATH`

Acceptance criteria:
- registry path can be passed explicitly
- missing registry is handled gracefully
- startup log shows whether registry was loaded

Dependencies:
- P1-01

### Task P1-03: Extend app state to include skill registry

Status:
- done

Workstream:
- WS2

Description:
- Pass registry access into request handlers through `AppState`.

Target files:
- `src/routes/mod.rs`

Acceptance criteria:
- request handlers can resolve active skills
- runtime does not reload registry per request
- registry access is thread-safe

Dependencies:
- P1-01
- P1-02

### Task P1-04: Implement Anthropic marker extraction

Status:
- done

Workstream:
- WS2

Description:
- Parse incoming Anthropic request payloads and extract the marker from the chosen transport location.

Target files:
- `src/routes/mod.rs`
- optionally `src/skills/resolver.rs`

Acceptance criteria:
- extraction works for the selected marker format
- no marker means clean fallback behavior
- malformed marker is logged and ignored safely

Dependencies:
- P0-04
- P1-03

### Task P1-05: Introduce resolved bridge context

Status:
- done

Workstream:
- WS2

Description:
- Create an internal struct that represents a resolved skill mapping before translation.

Suggested fields:
- skill id
- version
- Codex artifact path
- merged instruction prefix
- tool alias table
- merge mode

Acceptance criteria:
- route layer can build a bridge context once per request
- translation layer can consume the context without knowing registry details

Dependencies:
- P1-03
- P1-04

## Phase 2: MVP End-to-End Skill Mapping

### Task P2-01: Add instruction composition API

Status:
- done

Workstream:
- WS2

Description:
- Update Anthropic-to-Codex translation flow so instructions can be enriched with a resolved skill context.

Target files:
- `src/translation/anthropic_to_codex.rs`

Acceptance criteria:
- translation still supports requests without skills
- skill-based instruction prefix is merged deterministically
- current behavior is preserved when no bridge context exists

Dependencies:
- P1-05

### Task P2-02: Define merge order and implement prepend mode

Status:
- done

Workstream:
- WS2

Description:
- Implement the initial merge policy for Codex instructions.

MVP rule:
- bridge-generated instruction first
- original system instruction next

Acceptance criteria:
- merge output is deterministic
- merge logic is unit tested
- no duplicated marker text leaks into final instructions unless explicitly intended

Dependencies:
- P2-01

### Task P2-03: Add artifact loader for Codex skill content

Status:
- done

Workstream:
- WS2

Description:
- Load generated Codex skill artifacts from disk so they can be used to enrich requests.

Acceptance criteria:
- runtime can load a `SKILL.md` or derived instruction payload
- missing artifacts produce explicit diagnostics
- artifact loading is isolated from route code

Dependencies:
- P1-05

### Task P2-04: Create one hand-authored MVP skill artifact

Status:
- done

Workstream:
- WS3

Description:
- Before building a full compiler, create one manual skill artifact to validate runtime plumbing.

Deliverables:
- one canonical skill folder
- one generated or hand-authored Codex `SKILL.md`
- one registry entry

Acceptance criteria:
- the runtime bridge can resolve and inject this skill end to end
- the artifact shape matches the intended future compiler output

Dependencies:
- P0-05
- P2-03

### Task P2-05: End-to-end Anthropic request test for MVP skill

Status:
- done

Workstream:
- WS6

Description:
- Add an integration test that simulates Claude activation and verifies the generated Codex request.

Acceptance criteria:
- marker is detected
- correct skill is resolved
- final `instructions` contains the mapped skill guidance
- request still serializes correctly

Dependencies:
- P2-02
- P2-04

### Task P2-06: Define unsupported skill fallback behavior

Status:
- done

Workstream:
- WS2

Description:
- Decide what happens when a skill is requested but is unsupported or not installed.

Options:
- ignore silently
- warn and continue without the skill
- fail request with explicit error

Recommendation:
- warn and continue for MVP

Acceptance criteria:
- behavior is deterministic
- behavior is documented
- logs are actionable

Dependencies:
- P2-05

## Phase 3: Skill Compiler

### Task P3-01: Create compiler package skeleton

Status:
- done

Workstream:
- WS3

Description:
- Add a package dedicated to turning canonical skill source into installable artifacts.

Suggested path:
- `packages/skill-compiler`

Acceptance criteria:
- package can be executed locally
- package layout supports schema validation and file generation

Dependencies:
- P0-02
- P0-03

### Task P3-02: Implement manifest validation

Status:
- done

Workstream:
- WS3

Description:
- Validate the canonical skill manifest before any artifact generation.

Acceptance criteria:
- invalid manifests fail fast
- errors point to the exact missing or invalid field
- validation is test-covered

Dependencies:
- P3-01

### Task P3-03: Generate Codex skill artifact

Status:
- done

Workstream:
- WS3

Description:
- Convert canonical skill source into a Codex-native bundle.

Minimum artifact:
- `SKILL.md`

Optional artifact:
- `agents/openai.yaml`

Acceptance criteria:
- output is deterministic
- generated `SKILL.md` follows Codex skill shape
- compiler can regenerate the same output idempotently

Dependencies:
- P3-02

### Task P3-04: Generate Claude plugin artifact

Status:
- done

Workstream:
- WS3

Description:
- Convert canonical skill source into a Claude-native plugin or skill activation bundle.

Likely outputs:
- plugin metadata
- command prompt template
- marker injection instructions

Acceptance criteria:
- artifact can activate the MVP skill in Claude
- marker strategy matches P0-04
- generated files are deterministic

Dependencies:
- P0-04
- P3-02

### Task P3-05: Generate proxy registry entry from compiler

Status:
- done

Workstream:
- WS3

Description:
- Remove manual registry maintenance by generating a registry entry from the same manifest.

Acceptance criteria:
- compiler emits registry data aligned with Rust schema
- generated registry points to generated artifact paths

Dependencies:
- P3-03
- P3-04

### Task P3-06: Add compiler fixture tests

Status:
- done

Workstream:
- WS6

Description:
- Verify canonical input -> generated outputs with golden fixtures.

Acceptance criteria:
- changing generated artifact output requires intentional fixture updates
- fixtures cover valid and invalid manifests

Dependencies:
- P3-05

## Phase 4: Installer CLI

### Task P4-01: Create CLI package skeleton

Status:
- done

Workstream:
- WS4

Description:
- Add a Node CLI package for installation and registry management.

Suggested path:
- `packages/skills-cli`

Acceptance criteria:
- package can run local dev commands
- command routing structure exists

Dependencies:
- none

### Task P4-02: Implement `add` command

Status:
- done

Workstream:
- WS4

Description:
- Install a skill source into local canonical storage, generate artifacts, and fan out to agent targets.

MVP support:
- local path source first

Later:
- GitHub shorthand
- Git URLs

Acceptance criteria:
- local source install works
- project install works
- target agents can be selected

Dependencies:
- P3-05
- P4-01

### Task P4-03: Implement local store layout and registry persistence

Status:
- done

Workstream:
- WS4

Description:
- Create a canonical store and generated registry for project installs.

Acceptance criteria:
- store layout is stable
- registry can be discovered by the Rust proxy
- repeated installs do not create ambiguous duplicate state

Dependencies:
- P4-02

### Task P4-04: Implement symlink install mode

Status:
- done

Workstream:
- WS4

Description:
- Link agent-specific install paths to canonical generated artifacts.

Acceptance criteria:
- Claude and Codex targets can be linked
- broken symlinks are detectable
- reinstall is idempotent

Dependencies:
- P4-02

### Task P4-05: Implement copy install fallback

Status:
- done

Workstream:
- WS4

Description:
- Provide a fallback install path where symlinks are unavailable or undesirable.

Acceptance criteria:
- CLI can switch between symlink and copy
- copy installs preserve artifact correctness

Dependencies:
- P4-02

### Task P4-06: Implement `list` command

Status:
- done

Workstream:
- WS4

Description:
- Show installed skills, scopes, and agents.

Acceptance criteria:
- output distinguishes project vs global
- output distinguishes installed agents

Dependencies:
- P4-03

### Task P4-07: Implement `remove` command

Status:
- done

Workstream:
- WS4

Description:
- Remove installed agent artifacts and registry references cleanly.

Acceptance criteria:
- remove does not break unrelated skills
- store cleanup behavior is defined

Dependencies:
- P4-03

### Task P4-08: Implement `update` command

Status:
- done

Workstream:
- WS4

Description:
- Rebuild artifacts and refresh linked installs for existing skills.

Acceptance criteria:
- update is idempotent
- update preserves registry consistency

Dependencies:
- P4-03

### Task P4-09: Implement `doctor` command

Status:
- done

Workstream:
- WS4

Description:
- Validate store integrity, generated artifacts, symlink health, and registry correctness.

Acceptance criteria:
- command identifies missing artifacts
- command identifies broken symlinks
- command identifies registry mismatch

Dependencies:
- P4-03
- P4-04

## Phase 5: Claude Plugin Adapter

### Task P5-01: Define Claude plugin template structure

Status:
- done

Workstream:
- WS5

Description:
- Choose the minimal Claude plugin artifact shape needed for MVP.

Acceptance criteria:
- plugin structure supports marker injection
- plugin structure supports one skill activation path

Dependencies:
- P0-04

### Task P5-02: Implement one Claude activation command

Status:
- done

Workstream:
- WS5

Description:
- Provide one command or prompt path that activates the MVP skill and injects the marker.

Acceptance criteria:
- activation path is easy to demonstrate
- marker presence is predictable
- command does not depend on free-form user discipline

Dependencies:
- P5-01
- P3-04

### Task P5-03: Add plugin packaging and install verification

Status:
- done

Workstream:
- WS5

Description:
- Ensure the generated Claude artifact lands in the correct install target and can be reloaded or discovered by Claude.

Acceptance criteria:
- project installation is reproducible
- plugin package contains only required generated pieces

Dependencies:
- P4-04
- P5-02

## Phase 6: Tool Mapping and References

### Task P6-01: Extend registry with tool alias mappings

Status:
- done

Workstream:
- WS2

Description:
- Support skill-specific tool name translation at request time.

Acceptance criteria:
- alias table can be loaded from registry
- alias resolution is opt-in per skill

Dependencies:
- P0-03

### Task P6-02: Add tool alias resolution to translation flow

Status:
- done

Workstream:
- WS2

Description:
- Extend tool normalization so skill-specific aliases can be applied before Codex request emission.

Target files:
- `src/translation/tool_format.rs`

Acceptance criteria:
- mapped tools preserve schema and name consistency
- existing behavior is unchanged when no aliases exist

Dependencies:
- P6-01

### Task P6-03: Define reference loading policy

Status:
- done

Workstream:
- WS1

Description:
- Decide how and when large reference files are pulled into the instruction context.

Options:
- inline excerpts only
- lazy load on every request
- precompiled summary

Recommendation:
- precompile selected excerpts for MVP

Acceptance criteria:
- policy limits context bloat
- policy is deterministic and explainable

Dependencies:
- P0-02

### Task P6-04: Implement reference packaging

Status:
- done

Workstream:
- WS3

Description:
- Make the compiler generate the reference payload expected by the runtime.

Acceptance criteria:
- runtime does not need to parse arbitrary long reference docs ad hoc
- generated output stays small enough for request-time composition

Dependencies:
- P6-03

### Task P6-05: Add reference-aware runtime injection

Status:
- done

Workstream:
- WS2

Description:
- Include selected reference payloads when composing Codex instructions.

Acceptance criteria:
- context stays bounded
- injection order is deterministic

Dependencies:
- P6-04

## Phase 7: Quality, Operations, and Hardening

### Task P7-01: Add structured logs for skill resolution

Status:
- done

Workstream:
- WS7

Description:
- Improve observability for marker extraction, registry hits, misses, and applied mappings.

Acceptance criteria:
- logs expose traceable skill resolution
- logs are useful without leaking excessive prompt content

Dependencies:
- P1-04
- P1-05

### Task P7-02: Add regression coverage for fallback cases

Status:
- done

Workstream:
- WS6

Description:
- Protect unsupported skill and missing registry behavior from accidental breakage.

Acceptance criteria:
- fallback behavior is test-covered
- malformed markers are test-covered

Dependencies:
- P2-06

### Task P7-03: Add example repository walkthrough

Status:
- done

Workstream:
- WS7

Description:
- Provide a simple example showing install, activation, proxy resolution, and result.

Acceptance criteria:
- a new contributor can follow the happy path
- the docs align with implemented commands

Dependencies:
- P4-02
- P5-03

## Cross-Cutting Questions to Resolve During Execution

### Q1: Where should canonical skills live

Options:
- inside this repository
- in a dedicated sibling repository
- both, with local development support

Decision impact:
- installer source support
- versioning model
- contribution workflow

### Q2: How much of Codex skill output should be generated versus templated

Decision impact:
- compiler complexity
- diff quality
- maintainability

### Q3: Should global install be blocked until project install is stable

Recommendation:
- yes

Reason:
- project-local state is easier to debug first

### Q4: Should OpenAI request path support skill mapping too

Recommendation:
- no for MVP

Reason:
- Anthropic path is the primary bridge use case

## Suggested Execution Order

The recommended implementation order is:

1. P0-01 through P0-05
2. P1-01 through P1-05
3. P2-01 through P2-06
4. P3-01 through P3-06
5. P4-01 through P4-06
6. P5-01 through P5-03
7. P6-01 through P6-05
8. P7-01 through P7-03

This order deliberately proves the runtime bridge before building the full installer.

## MVP Exit Checklist

The MVP is complete only when all of the following are true:

- one canonical skill exists
- one generated Claude activation artifact exists
- one generated Codex skill artifact exists
- one generated registry entry exists
- the Rust proxy can load the registry
- the Anthropic request path can extract the marker
- the proxy resolves the skill and injects Codex instructions
- at least one end-to-end test proves the mapping

## Post-MVP Expansion Backlog

- support global install
- support remote repository install sources
- support multiple skills per source repo
- support Cursor adapter
- support reference-heavy skills
- support tool schema transforms
- support version conflict resolution
- support richer `doctor` diagnostics

## Recommended Owner Split

If work is parallelized, split ownership like this:

- Owner A: Rust runtime bridge
- Owner B: compiler and schemas
- Owner C: installer CLI and Claude plugin packaging
- Owner D: test fixtures and example flows

If one person is doing the work, still use the same boundary lines to avoid mixing concerns too early.

## Notes

- Do not start with universal agent support.
- Do not rely on prompt inference to detect skills.
- Do not let Rust own packaging concerns.
- Do not build remote source fetching before local source install works.

The fastest path is a thin but correct vertical slice, then expansion.
