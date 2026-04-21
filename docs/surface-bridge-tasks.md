# Surface Bridge — Implementation Tasks

> Generated from `docs/claude-codex-capability-bridge-plan.md` (v4-final, architecture-frozen).
> 
> **Statuses:** `todo` · `in-progress` · `blocked` · `done` · `cut`
> **Blocked format:** `blocked(TASK-ID)` — task cannot start until dependency is done.

---

## Phase -1 — Bootstrap & Doctor

### CLI Scaffolding

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| B-001 | Add `clap` subcommand routing: `setup`, `doctor`, `env`, `serve` (default) | `src/main.rs`, `src/cli/mod.rs` | done | — | `serve` = current behavior; other subcommands are new |
| B-002 | Detect `codex` binary in PATH, parse `codex --version` | `src/cli/setup.rs` | done | B-001 | Return structured `CodexBinaryInfo` |
| B-003 | Check `codex login` auth state (not `auth.json`) | `src/cli/setup.rs` | done | B-002 | Implemented via app-server auth/account probes |
| B-004 | Spawn `codex app-server` + JSON-RPC `initialize` handshake (smoke) | `src/cli/setup.rs` | done | B-002 | Reuse handshake logic from Phase 1 |
| B-005 | Call `configRequirements/read` to discover allowed policies/sandbox modes | `src/cli/setup.rs` | done | B-004 | Report now includes requirements |
| B-006 | Smoke test via `command/exec ["pwd"]` to confirm sandbox works | `src/cli/setup.rs` | done | B-004 | Validates sandbox path without a full thread |
| B-007 | Model discovery via handshake, print available models | `src/cli/setup.rs` | done | B-004 | |
| B-008 | Print Claude-compatible endpoint config snippet (JSON) | `src/cli/setup.rs` | done | B-007 | `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL` |
| B-009 | `setup --write-config` — auto-write config to common client locations | `src/cli/setup.rs` | done | B-008 | Detects Claude Code settings paths and merges env config |
| B-010 | `doctor` — report transport, codex version, auth, API stability, surface tiers | `src/cli/doctor.rs` | done | B-002 | |
| B-011 | `doctor` — call `configRequirements/read`, report allowed policies/features | `src/cli/doctor.rs` | done | B-004 | |
| B-012 | `doctor` — report degraded surfaces with `AvailabilityGate` reasons | `src/cli/doctor.rs` | done | P0-003 | Needs surface registry with availability gates |
| B-013 | `doctor --json` — machine-readable output | `src/cli/doctor.rs` | done | B-010 | |
| B-014 | `env` — output Claude client config snippet | `src/cli/env.rs` | done | B-001 | |
| B-015 | `env --shell bash\|zsh\|fish\|powershell` — correct export format | `src/cli/env.rs` | done | B-014 | |

### Exit Criteria Tests

| ID | Task | Status | Depends |
|---|---|---|---|
| B-T01 | Test: setup completes ≤ 3 steps on clean machine with `codex` installed | done | B-009 |
| B-T02 | Test: `doctor --json` outputs valid JSON with all required fields | done | B-013 |
| B-T03 | Test: `env --shell zsh` outputs correct `export` statements | done | B-015 |

---

## Phase 0 — Surface Model + Matrix

### Documentation

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P0-001 | Write `docs/surfaces/claude-inventory.md` — all Claude surfaces with buckets | `docs/surfaces/` | done | — | Source: plan inventory tables |
| P0-002 | Write `docs/surfaces/codex-target-inventory.md` — Codex app-server surfaces | `docs/surfaces/` | done | — | |
| P0-003 | Write `docs/surfaces/compatibility-matrix.md` — full matrix (tier+bucket+strategy+fallback) | `docs/surfaces/` | done | P0-001, P0-002 | |

### Rust Types

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P0-010 | Define `SurfaceKind` enum | `src/surfaces/model.rs` | done | — | Tool, Command, Skill, Workflow, StateSurface, HostIntegration |
| P0-011 | Define `SurfaceFamily` enum | `src/surfaces/model.rs` | done | — | FileCode, Execution, SearchWeb, Review, Jobs, Planning, Workspace, Scheduling, DurableRoutines, GuidanceMemory, ConfigPermissions, Mcp, Subagents, CodeIntelligence, Interaction, Teams, Meta, Observability, Notebook, UiMisc |
| P0-012 | Define `SurfaceBucket` enum | `src/surfaces/model.rs` | done | — | RuntimeCritical, WorkflowRuntime, HostAdminUx, PlatformSpecific, OutOfScope |
| P0-013 | Define `MappingStrategy` enum | `src/surfaces/model.rs` | done | — | Native, MediatedNative, WorkflowEmulated, UnsupportedExplicit |
| P0-014 | Define `FallbackMode` enum | `src/surfaces/model.rs` | done | — | HardError, SoftWarningAndContinue, DowngradeToWorkflow, DropWithObservability |
| P0-015 | Define `OperationMode` enum | `src/surfaces/model.rs` | done | — | StrictAppServer, AutoHybrid, ResponsesOnly |
| P0-016 | Define `StateScope`, `SideEffectLevel`, `AsyncMode`, `ApprovalSensitivity`, `HostDependency`, `InvocationMode` enums | `src/surfaces/model.rs` | done | — | |
| P0-017 | Define `AvailabilityGate` struct | `src/surfaces/model.rs` | done | — | min_version, env_flags, required_plugins, required_binaries, platform, plan_or_product, experimental |
| P0-018 | Define `SurfaceDescriptor` struct | `src/surfaces/model.rs` | done | P0-010..P0-017 | |
| P0-019 | Define `MappingDecision` struct | `src/surfaces/model.rs` | done | P0-013..P0-015 | |
| P0-020 | Define `UnsupportedReason` enum | `src/surfaces/model.rs` | done | — | MissingBackendPrimitive, StateModelMismatch, ApprovalModelMismatch, DeprecatedSourceSurface, HostDependencyGap, PlatformSpecificGap |
| P0-030 | Build static `SurfaceRegistry` with all surfaces from inventory | `src/surfaces/registry.rs` | done | P0-018 | One `SurfaceDescriptor` per Claude surface |
| P0-031 | Build `CompatibilityMatrix` — lookup `MappingDecision` by surface_id + operation_mode | `src/surfaces/matrix.rs` | done | P0-019, P0-030 | |
| P0-032 | Implement `SurfaceClassifier` — classify incoming request to `SurfaceDescriptor` | `src/surfaces/classifier.rs` | done | P0-030 | Detect tool calls, slash commands, skill markers |
| P0-033 | Wire `src/surfaces/mod.rs` | `src/surfaces/mod.rs` | done | P0-030..P0-032 | |

### Tests

| ID | Task | Status | Depends |
|---|---|---|---|
| P0-T01 | Unit: every registered surface has tier, bucket, strategy, fallback | done | P0-030 |
| P0-T02 | Unit: `host_admin_ux`/`out_of_scope` surfaces have `DropWithObservability` fallback | done | P0-031 |
| P0-T03 | Unit: classifier correctly identifies `Read`, `Bash`, `/plan`, `TaskCreate`, unknown | done | P0-032 |

---

## Phase 1 — App-server Foundation + Approval

### JSON-RPC Transport

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P1-001 | Define JSON-RPC 2.0 message types (Request, Response, Notification, Error) | `src/app_server/jsonrpc.rs` | done | — | |
| P1-002 | Implement stdio transport: spawn `codex app-server`, read/write framed JSON-RPC | `src/app_server/transport_stdio.rs` | done | P1-001 | Async tokio child process |
| P1-003 | Implement `initialize` / `initialized` handshake with capability discovery | `src/app_server/handshake.rs` | done | P1-002 | Include `apiStability` flag (stable/experimental) |
| P1-004 | Call `configRequirements/read` post-handshake, store allowed policies | `src/app_server/handshake.rs` | done | P1-003 | Feeds into Approval Bridge |

### Thread / Turn / Item State

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P1-010 | Define `BridgeThread` struct (thread_id, cwd, approval_policy, sandbox_config, turn_count) | `src/app_server/thread.rs` | done | — | |
| P1-011 | Define `BridgeTurn` struct (turn_id, thread_id, role, status, items, pending_interaction) | `src/app_server/thread.rs` | done | — | `TurnStatus`: Active, PausedForApproval, PausedForClarification, Completed, Failed |
| P1-012 | Define `BridgeItemRef` struct (item_id, item_type, surface_id) | `src/app_server/thread.rs` | done | — | |
| P1-013 | Define `BridgeSession` struct wrapping thread + transport + config | `src/app_server/session.rs` | done | P1-010 | Includes OperationMode, ApiStability, DelegationPolicy |
| P1-014 | Implement `thread/start` with `approvalPolicy` + `sandbox` params | `src/app_server/client.rs` | done | P1-002, P1-010 | |
| P1-015 | Implement `turn/start` with optional per-turn policy override | `src/app_server/client.rs` | done | P1-014 | |
| P1-016 | Implement streamed event receiving (items, deltas, status changes) | `src/app_server/events.rs` | done | P1-002 | Parse JSON-RPC notifications |
| P1-017 | Implement `turn/cancel` | `src/app_server/client.rs` | done | P1-015 | Implemented as `turn/interrupt` |

### Approval Bridge

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P1-020 | Define `ApprovalPolicy` and `SandboxConfig` types | `src/mapping/approvals.rs` | done | — | |
| P1-021 | Implement policy precedence: thread default vs turn override (tighten-only) | `src/mapping/approvals.rs` | done | P1-020, P1-004 | Turn cannot be more permissive than thread |
| P1-022 | Handle server-initiated approval pause: detect `waitingOnApproval`, surface to client | `src/mapping/approvals.rs` | done | P1-016 | |
| P1-023 | Handle approval response: client allow/deny → resume turn | `src/mapping/approvals.rs` | done | P1-022 | |
| P1-024 | `/sandbox` surface mapping: translate Claude sandbox intent to `thread/start` params | `src/mapping/approvals.rs` | done | P1-014, P0-031 | |

### User Interaction Bridge

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P1-030 | Define `UserInteractionKind` enum (ClarificationQuestion, ApprovalRequest) | `src/mapping/interaction.rs` | done | — | |
| P1-031 | Define `UserInteractionBridge` struct (interaction_id, kind, turn_id, surface_id, status) | `src/mapping/interaction.rs` | done | P1-030 | |
| P1-032 | Implement clarification pause/resume (separate from approval) | `src/mapping/interaction.rs` | done | P1-031, P1-016 | |

### Operation Modes + API Stability

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P1-040 | CLI flags: `--mode strict-app-server\|auto-hybrid\|responses-only` | `src/main.rs` | done | B-001 | Default: `auto-hybrid` |
| P1-041 | CLI flags: `--app-server-stable` (default), `--app-server-experimental` | `src/main.rs` | done | B-001 | Sets `experimentalApi` in handshake |
| P1-042 | `auto-hybrid` fallback: detect app-server unavailable, route to Responses API | `src/routes/mod.rs` | done | P1-002 | Reuse existing `codex_client.rs` path |
| P1-043 | `DelegationPolicy` enum + storage in `BridgeSession` | `src/app_server/session.rs` | done | P1-013 | Default: ExplicitOnly |

### Integration Wiring

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P1-050 | Wire app-server client into `src/main.rs` startup alongside existing `CodexClient` | `src/main.rs` | done | P1-002, P1-013 | |
| P1-051 | Add surface-aware dispatch in `src/routes/mod.rs` alongside existing handlers | `src/routes/mod.rs` | done | P0-032, P1-050 | |

### Tests

| ID | Task | Status | Depends |
|---|---|---|---|
| P1-T01 | Integration: spawn app-server → handshake → start thread → start turn → receive items | done | P1-016 | P1-016 |
| P1-T02 | Integration: approval pause → client allows → turn resumes | done | P1-023 | P1-023 |
| P1-T03 | Integration: clarification pause → client answers → turn resumes | done | P1-032 | P1-032 |
| P1-T04 | Unit: turn-level override rejected when looser than thread policy | done | P1-021 |
| P1-T05 | Integration: `auto-hybrid` falls back to Responses when app-server unavailable | done | P1-042 | P1-042 |
| P1-T06 | Unit: stable API default; experimental opt-in sets capability flag | done | P1-041 |

---

## Phase 2 — Core Tool Parity + Permissions (Tier 0 + partial Tier 2)

### Tool Mapping

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P2-001 | Implement `Read` mapping (native) | `src/mapping/tools.rs` | done | P1-051 | |
| P2-002 | Implement `Write` mapping (mediated_native) + approval enforcement | `src/mapping/tools.rs` | done | P1-024 | |
| P2-003 | Implement `Edit` mapping (mediated_native) + protected-path check | `src/mapping/tools.rs` | done | P1-024 | |
| P2-004 | Implement `MultiEdit` mapping + atomicity warning | `src/mapping/tools.rs` | done | P2-003 | |
| P2-005 | Implement `Glob` mapping (native) | `src/mapping/tools.rs` | done | P1-051 | |
| P2-006 | Implement `Grep` mapping (native) | `src/mapping/tools.rs` | done | P1-051 | |
| P2-007 | Implement `LS` mapping (native) | `src/mapping/tools.rs` | done | P1-051 | |
| P2-008 | Implement `Bash` mapping (mediated_native) + approval + cwd continuity | `src/mapping/tools.rs` | done | P1-024 | Must preserve session/thread cwd |
| P2-009 | Path/cwd normalization helper: resolve Claude paths relative to `BridgeThread.cwd` | `src/mapping/tools.rs` | done | P1-010 | |

### Permissions

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P2-020 | `/permissions` surface: load approval profile, apply to thread | `src/mapping/approvals.rs` | done | P1-024 | Minimum viable permission profile mapping |
| P2-021 | Store permission profiles in `src/state/permissions.rs` | `src/state/permissions.rs` | done | P2-020 | |

### Golden Fixtures

| ID | Task | Status | Depends |
|---|---|---|---|
| P2-F01 | Fixture: `Read` — Claude request → expected Codex mapping | done | P2-001 |
| P2-F02 | Fixture: `Write` — approval enforced, overwrite semantics preserved | done | P2-002 |
| P2-F03 | Fixture: `Edit` — patch granularity, protected path | done | P2-003 |
| P2-F04 | Fixture: `MultiEdit` — atomicity warning emitted | done | P2-004 |
| P2-F05 | Fixture: `Glob` | done | P2-005 |
| P2-F06 | Fixture: `Grep` | done | P2-006 |
| P2-F07 | Fixture: `LS` | done | P2-007 |
| P2-F08 | Fixture: `Bash` — approval + cwd preserved across turns | done | P2-008 |

### Exit Criteria Tests

| ID | Task | Status | Depends |
|---|---|---|---|
| P2-T01 | All 8 golden fixtures pass (100%) | done | P2-F01..F08 |
| P2-T02 | Structured downgrade warnings emitted for mediated_native surfaces | done | P2-002 |
| P2-T03 | Shell flow preserves thread continuity (cwd, env) | done | P2-008 |

---

## Phase 3 — Tasks + Subagents + Review (Tier 1)

### Task Subsystem

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P3-001 | Define `JobRecord` struct with `JobKind`, `JobStatus`, `SchedulerMode` | `src/jobs/model.rs` | done | — | |
| P3-002 | Implement `JobRegistry` — in-memory job tracking (CRUD) | `src/jobs/registry.rs` | done | P3-001 | |
| P3-003 | `TaskCreate` mapping → spawn Codex thread/subagent, create `JobRecord` | `src/mapping/tasks.rs` | done | P1-014, P3-002 | |
| P3-004 | `TaskGet` mapping → query `JobRegistry` + thread state | `src/mapping/tasks.rs` | done | P3-002 | |
| P3-005 | `TaskList` mapping → list from `JobRegistry` | `src/mapping/tasks.rs` | done | P3-002 | |
| P3-006 | `TaskUpdate` mapping → inject turn into task thread | `src/mapping/tasks.rs` | done | P1-015, P3-002 | |
| P3-007 | `TaskStop` mapping → cancel thread/job, structured result | `src/mapping/tasks.rs` | done | P1-017, P3-002 | |
| P3-008 | `/tasks` command → list active jobs | `src/mapping/commands.rs` | done | P3-005 | |

### Subagents

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P3-010 | `Agent` mapping + `DelegationPolicy` enforcement | `src/mapping/subagents.rs` | done | P1-043 | Reject if policy is `Never`; enforce `ExplicitOnly` default |
| P3-011 | `SendMessage` mapping → inter-agent message via thread | `src/mapping/subagents.rs` | done | P3-010 | |
| P3-012 | Child/subagent approval isolation: child approval doesn't break parent flow | `src/mapping/subagents.rs` | done | P1-022, P3-010 | |

### Review Family

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P3-020 | `code_review` workflow → spawn review agent, create `JobRecord` | `src/mapping/review.rs`, `src/jobs/review.rs` | done | P3-002 | Output: findings, file refs, severity, changes |
| P3-021 | `security_review` workflow → review agent + security-biased spec | `src/mapping/review.rs` | done | P3-020 | |
| P3-022 | `rescue_fix` workflow → subagent/job spawn, may use `thread/fork` | `src/mapping/review.rs`, `src/jobs/rescue.rs` | done | P3-010 | |
| P3-023 | `review_status` → query `JobRegistry` for review jobs | `src/mapping/review.rs` | done | P3-002 | |
| P3-024 | `review_cancel` → cancel review job | `src/mapping/review.rs` | done | P3-002 | |
| P3-025 | `/security-review` command → trigger `security_review` workflow | `src/mapping/commands.rs` | done | P3-021 | |

### Interaction

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P3-030 | Wire `AskUserQuestion` → `UserInteractionBridge.ClarificationQuestion` | `src/mapping/interaction.rs` | done | P1-032 | Must NOT dispatch as approval |

### Tests

| ID | Task | Status | Depends |
|---|---|---|---|
| P3-T01 | Fixture: `TaskCreate` → job created, thread spawned | done | P3-003 |
| P3-T02 | Fixture: `TaskGet` / `TaskList` reflect registry state | done | P3-004 |
| P3-T03 | Fixture: `Agent` with `DelegationPolicy.ExplicitOnly` | done | P3-010 |
| P3-T04 | Fixture: `Agent` with `DelegationPolicy.Never` → rejected | done | P3-010 |
| P3-T05 | Fixture: `code_review` end-to-end with status/result/cancel | done | P3-020 |
| P3-T06 | Fixture: `AskUserQuestion` dispatched as clarification, not approval | done | P3-030 |
| P3-T07 | Integration: child subagent approval isolated from parent | done | P3-012 | P3-012 |
| P3-T08 | Fixture: `rescue_fix` uses `thread/fork` | done | P3-022 | P3-022 |

---

## Phase 4 — Planning + Workspace (Tier 2)

### Planning

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P4-001 | `EnterPlanMode` mapping → instruction/profile switch + `item/plan/delta` awareness | `src/mapping/planning.rs` | done | P1-016 | `mediated_native` |
| P4-002 | `ExitPlanMode` mapping → switch back to execution mode | `src/mapping/planning.rs` | done | P4-001 | |
| P4-003 | `/plan` command → instruction injection + plan item surfacing | `src/mapping/planning.rs` | done | P4-001 | |
| P4-004 | Event translator: surface `item/plan/delta` events in Claude-compatible format | `src/adapters/claude_output.rs` | done | P1-016, P4-001 | |

### Workspace

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P4-010 | `EnterWorktree` mapping → **hybrid orchestration**: thread state (native) + git worktree (bridge-orchestrated) | `src/mapping/workspace.rs` | done | P1-014 | Bridge manages git worktree lifecycle via Bash/git policy |
| P4-011 | `ExitWorktree` mapping → cleanup git worktree + return thread to main | `src/mapping/workspace.rs` | done | P4-010 | |
| P4-012 | `/resume` → `thread/resume` | `src/mapping/workspace.rs` | done | P1-014 | |
| P4-013 | `/rewind` → `thread/rollback` (preferred over re-creation) | `src/mapping/workspace.rs` | done | P1-014 | |
| P4-014 | `thread/fork` for branch work (rescue, review, exploratory) | `src/mapping/workspace.rs` | done | P1-014 | |
| P4-015 | Worktree ↔ thread association stored in `BridgeSession.active_jobs` or dedicated map | `src/state/store.rs` | done | P4-010 | |

### Tests

| ID | Task | Status | Depends |
|---|---|---|---|
| P4-T01 | Fixture: `EnterPlanMode` → `item/plan/delta` events surface correctly | done | P4-004 |
| P4-T02 | Fixture: `EnterWorktree` → git worktree created, thread associated | done | P4-010 | P4-010 |
| P4-T03 | Fixture: `/resume` → paused thread resumes with state intact | done | P4-012 | P4-012 |
| P4-T04 | Fixture: `/rewind` → `thread/rollback`, not re-creation | done | P4-013 | P4-013 |

---

## Phase 5 — Scheduling + Intelligence + Web (Tier 3)

### Session Scheduling

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P5-001 | Define `SchedulingSurface` enum (SessionCron, DurableRoutine) | `src/jobs/model.rs` | done | P3-001 | |
| P5-002 | Define `SchedulerMode` enum (SessionCron, DurableAutomation) | `src/jobs/model.rs` | done | P3-001 | |
| P5-003 | `CronCreate` mapping → `SessionCron` by default; `DurableAutomation` with warning on explicit request | `src/mapping/scheduling.rs` | done | P3-002 | |
| P5-004 | `CronList` mapping → query scheduler registry | `src/mapping/scheduling.rs` | done | P5-003 | |
| P5-005 | `CronDelete` mapping → remove scheduler entry | `src/mapping/scheduling.rs` | done | P5-003 | |
| P5-006 | Session-scoped scheduler: entries die when session ends | `src/jobs/scheduler.rs` | done | P5-003 | |
| P5-007 | `/schedule` → `unsupported_explicit` with structured reason (durable routine, no Codex equivalent) | `src/mapping/commands.rs` | done | P0-019 | |

### Intelligence + Web

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P5-010 | `ToolSearch` → `mediated_native`: capability handshake + local deferred surface registry | `src/mapping/tools.rs` | done | P1-003 | Partial emulation only |
| P5-011 | `WebFetch` mapping → native or mediated_native depending on Codex runtime | `src/mapping/tools.rs` | done | P1-051 | |
| P5-012 | `WebSearch` mapping → mediated_native, align filtering | `src/mapping/tools.rs` | done | P1-051 | |
| P5-013 | `Monitor` → workflow_emulated via polling/event subscription | `src/mapping/tools.rs` | done | P1-016 | |

### Tests

| ID | Task | Status | Depends |
|---|---|---|---|
| P5-T01 | Fixture: `CronCreate` with `SessionCron` — entry ephemeral | done | P5-003 |
| P5-T02 | Fixture: `CronCreate` with `DurableAutomation` — warning emitted | done | P5-003 |
| P5-T03 | Fixture: `/schedule` returns structured unsupported reason | done | P5-007 |
| P5-T04 | Fixture: `WebFetch` works where runtime exposes it | done | P5-011 | P5-011 |

---

## Phase 6 — Guidance + Skills + MCP (Tier 4)

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P6-001 | `/init` workflow → inspect repo → propose `AGENTS.md` | `src/mapping/guidance.rs` | done | P1-051 | |
| P6-002 | `/memory` workflow → import from `CLAUDE.md`, proposal-first, no auto-sync | `src/mapping/guidance.rs` | done | P1-051 | |
| P6-003 | Guidance layer state storage | `src/state/guidance.rs` | done | P6-001 | |
| P6-004 | `/mcp` → read/write Codex `config.toml` MCP bridge | `src/mapping/commands.rs` | done | P1-051 | Codex CLI/IDE share config |
| P6-005 | `/plugin` → skill install, normalize to Codex skill model | `src/mapping/commands.rs` | done | P1-051 | Extends `src/skills/` |
| P6-006 | `NotebookRead` mapping → mediated_native document read | `src/mapping/tools.rs` | done | P1-051 | |
| P6-007 | `NotebookEdit` → `unsupported_explicit` | `src/mapping/tools.rs` | done | P0-019 | |

### Tests

| ID | Task | Status | Depends |
|---|---|---|---|
| P6-T01 | Fixture: `memory_import` — `CLAUDE.md` → guidance proposal | done | P6-002 |
| P6-T02 | Fixture: `init_guidance_bootstrap` — `AGENTS.md` generated | done | P6-001 |
| P6-T03 | No auto-sync between Claude memory and Codex guidance | done | P6-002 |

---

## Phase 7 — Hardening + Regression

| ID | Task | File(s) | Status | Depends | Notes |
|---|---|---|---|---|---|
| P7-001 | Generate `schema_stable.rs` from `codex app-server generate-json-schema` | `src/app_server/schema_stable.rs` | done | P1-003 | Pin to exact binary version |
| P7-002 | Generate `schema_experimental.rs` from experimental schema | `src/app_server/schema_experimental.rs` | done | P7-001 | |
| P7-003 | `GET /bridge/compatibility` endpoint | `src/routes/mod.rs` | done | P0-031 | Return full matrix as JSON |
| P7-004 | `GET /bridge/surfaces` endpoint | `src/routes/mod.rs` | done | P0-030 | |
| P7-005 | `GET /bridge/surfaces/:id` endpoint | `src/routes/mod.rs` | done | P0-030 | |
| P7-006 | `GET /bridge/jobs` endpoint | `src/routes/mod.rs` | done | P3-002 | Current implementation exposes in-memory registry state |
| P7-007 | `GET /bridge/session/:id` endpoint | `src/routes/mod.rs` | done | P1-013 | |
| P7-008 | `GET /bridge/mode` endpoint | `src/routes/mod.rs` | done | P1-040 | Operation mode + API stability + delegation + degradation |
| P7-009 | Bridge metadata in every response (`bridge: { surface_id, strategy, ... }`) | `src/adapters/claude_output.rs` | done | P0-019 | |
| P7-010 | Degradation telemetry: log every downgrade decision with surface context | `src/observability/traces.rs` | done | P0-031 | |
| P7-011 | Full regression suite: all tier fixtures, all acceptance metrics | `tests/` | done | P2-T01, P3-T01..T08, P4-T01..T04, P5-T01..T04, P6-T01..T03 | |
| P7-012 | `doctor --json` output matches actual capability profile post-hardening | `src/cli/doctor.rs` | done | P7-001, B-013 | |
| P7-013 | Update README to reflect app-server-first architecture | `README.md` | done | P1-T01 | **Gated on Phase 1 e2e pass** |

### Acceptance Gate

| ID | Task | Status | Depends |
|---|---|---|---|
| P7-A01 | 100% Tier 0 fixtures pass | done | P2-T01 |
| P7-A02 | ≥ 80% Tier 1-2 fixtures pass | done | P3-T01..T08, P4-T01..T04 | P3-T01..T08, P4-T01..T04 |
| P7-A03 | 0 silent downgrades on side-effect surfaces | done | P7-010 |
| P7-A04 | 0 `AskUserQuestion` dispatched as approval | done | P3-T06 |
| P7-A05 | `DelegationPolicy.ExplicitOnly` enforced in all Tier 1 tests | done | P3-T03 |
| P7-A06 | Session resume preserves thread/cwd/guidance/approval state | done | P4-T03 | P4-T03 |
| P7-A07 | Setup → first session ≤ 3 steps | done | B-T01 |

---

## Summary

| Phase | Tasks | Status |
|---|---|---|
| Phase -1: Bootstrap & Doctor | 18 | **18 done** |
| Phase 0: Surface Model + Matrix | 16 | **16 done** |
| Phase 1: App-server Foundation + Approval | 28 | **28 done** |
| Phase 2: Core Tool Parity + Permissions | 22 | **22 done** |
| Phase 3: Tasks + Subagents + Review | 24 | **24 done** |
| Phase 4: Planning + Workspace | 13 | **13 done** |
| Phase 5: Scheduling + Intelligence + Web | 11 | **11 done** |
| Phase 6: Guidance + Skills + MCP | 10 | **10 done** |
| Phase 7: Hardening + Regression | 20 | **20 done** |
| **Total** | **162** | **162 done** |