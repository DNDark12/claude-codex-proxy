# Claude to Codex Surface Bridge Plan — App-Server-First (v4-final)

## Status

Production plan, architecture-frozen. Supersedes all prior drafts (capability bridge, surface bridge v1–v4).

Separate from `docs/claude-codex-skill-bridge-plan.md` (custom skill distribution).

## North Star

> **Maximize practical Claude Code power on top of Codex, with one-command local setup, transparent degradation, and app-server-native state/approval/job handling.**

### KPIs

- Clean machine to first successful session: **≤ 3 steps**
- Core tool loop fixtures: **100% pass**
- Advanced workflows: **tier-based coverage targets**
- Silent downgrades on side-effect surfaces: **0**

---

## Problem Statement

The current proxy (`src/proxy/codex_client.rs`) is a **stateless Responses API translator**:

```
Claude request → translate → POST chatgpt.com/.../codex/responses → translate back → response
```

This is insufficient because:

1. **Responses API is not the rich-client interface.** Codex `app-server` is designed for rich clients: bidirectional JSON-RPC, conversation history, approvals, streamed agent events.
2. **Claude Code power goes far beyond tool calls.** Tasks, plan mode, worktrees, scheduling, subagents, teams, monitors—these are what make Claude Code productive. Mapping only Read/Edit/Bash captures a fraction.
3. **Session state matters.** App-server organizes runtime around Thread/Turn/Item with server-initiated approval pause/resume. A stateless request translator cannot express this.
4. **Setup friction is high.** Current `--auth-path ~/.codex/auth.json` is implementation detail, not product UX. App-server mode should piggyback on `codex` CLI auth.

### What the current codebase does well

- `src/translation/` — Faithful Anthropic ↔ Codex ↔ OpenAI protocol translation
- `src/skills/resolver.rs` — Clean separation of skill detection from transport
- `src/proxy/event_extractor.rs` — Solid SSE streaming and event parsing
- `src/translation/tool_runtime.rs` — Malformed parameter recovery

### What must change

- Transport: Responses API → app-server JSON-RPC (stdio primary, websocket secondary)
- State: stateless request translation → Thread/Turn/Item-aware session
- Scope: tool parity → full surface coverage (tools + commands + workflows + jobs + guidance)
- Auth: `--auth-path` → piggyback on `codex` CLI login
- UX: manual config → `setup` / `doctor` / `env` commands
- Approval/sandbox: thread-lifecycle foundation, not a late-stage feature

---

## Architecture Decisions

### 1. App-server-first

OpenAI describes app-server as the interface for rich clients with authentication, conversation history, approvals, and streamed agent events. The proxy spawns `codex app-server` as a child process via stdio, communicates over JSON-RPC, and manages Thread/Turn/Item lifecycle natively.

Responses API remains as fallback for basic chat when app-server is unavailable.

### 2. Surface-first, not skill-first

Everything Claude Code exposes is a **surface**: built-in tool, built-in command, bundled skill, workflow, state surface, or host integration. Claude Code docs separate these clearly. Using "skill" as the primary unit would choose the wrong abstraction.

### 3. App-server-native state model

The proxy's internal state maps to app-server primitives first, then wraps with bridge abstractions:

| App-server primitive | Bridge wrapper | Purpose |
|---|---|---|
| Thread | `BridgeThread` | Conversation container |
| Turn | `BridgeTurn` | Single user→assistant round |
| Item | `BridgeItemRef` | Typed content within a turn |
| — | `BridgeSession` | Wraps thread + transport + config |
| — | `JobRecord` | Background work tracking |

This reduces impedance mismatch for streamed item notifications, approval pause/resume, review artifacts, and resume semantics.

### 4. Three operation modes

| Mode | Behavior |
|---|---|
| `strict-app-server` | No app-server = fail. All surfaces available. |
| `auto-hybrid` | App-server first; Responses API fallback for basic chat/tool-light. **Default.** |
| `responses-only` | Legacy/debug. Current behavior. |

Advanced surfaces return structured metadata when degraded. No silent parity claims.

### 5. Piggyback on Codex CLI auth

In app-server mode, `--auth-path` is **deprecated**. The proxy:
1. Checks for `codex` binary
2. Spawns `codex app-server` (inherits CLI auth state)
3. Performs JSON-RPC handshake (`initialize` → `initialized`)

`--auth-path` remains only for `responses-only` fallback mode.

### 6. Protocol drift mitigation

App-server protocol may change without notice (CLI docs caveat). Mitigation:
- **Pin `codex` binary version** in CI/release
- Use `codex app-server generate-json-schema` to regenerate protocol types from the exact binary shipped/tested
- Runtime **capability handshake**: discover supported methods, degrade gracefully for unknown ones
- Do not hand-write protocol structs long-term; generate from schema

### 7. Stable vs experimental API policy

Some app-server methods/fields are behind `capabilities.experimentalApi`. If a client uses experimental features without opt-in, the server rejects.

**V1 defaults to stable API surface only.** Experimental features require an explicit flag.

| Flag | Behavior |
|---|---|
| `--app-server-stable` | Default. Only stable methods/fields. |
| `--app-server-experimental` | Opt-in to `experimentalApi = true` in handshake. |

Generated schema should be split:
- `schema_stable.rs` — types from stable API surface
- `schema_experimental.rs` — types behind experimental flag

This makes binary pinning and regression testing tractable.

### 8. Approval is foundation, not feature

In app-server, `thread/start` already accepts `approvalPolicy` and `sandbox` directly. Runtime status includes `waitingOnApproval`. This means approval/sandbox is part of **thread lifecycle from the very first handshake**, not a layer added after tools work.

**Consequence:** Approval Bridge and `/sandbox` must be implemented in Phase 1 alongside the app-server foundation. Permission profile mapping must follow in Phase 2 at latest.

#### Policy precedence: `thread/start` vs `turn/start`

App-server allows both `thread/start` and `turn/start` to accept `approvalPolicy` and `sandboxPolicy`. The bridge must enforce clear precedence:

- **`thread/start`** sets session defaults for approval and sandbox policy.
- **`turn/start`** may override per-turn, but **only in the tightening direction** (v1 policy: a turn cannot be more permissive than its thread).
- If `configRequirements/read` reports that a policy/mode is disallowed, turn-level override attempting that policy is rejected.
- Override direction: `suggest-then-approve` → `approve-always` is allowed (tighter). `approve-always` → `auto-approve` is rejected (looser).

This prevents security bugs where a single turn escapes the thread's sandbox or approval boundary.

### 9. Deterministic delegation policy for subagents

Codex spawns subagents **only when explicitly requested**. Claude's `Agent` surface could mislead toward auto-delegation. The bridge must enforce a deterministic policy.

```rust
pub enum DelegationPolicy {
    /// Never spawn subagents, even if requested
    Never,
    /// Only spawn when Claude explicitly invokes Agent/SendMessage
    ExplicitOnly,
    /// Heuristic: bridge may spawn based on surface classification
    Heuristic,
    /// Always spawn for specific surface families
    ForceForSurface(Vec<SurfaceFamily>),
}
```

**Default: `ExplicitOnly`.** Only open `Heuristic` when conformance tests for Tier 1 are passing.

---

## Surface Classification Buckets

Not all Claude surfaces are equally important to the bridge. To prevent backlog drift toward low-value work, every surface is classified into one of five buckets:

| Bucket | Description | Action |
|---|---|---|
| `runtime_critical` | Core agent loop: tools that read/write/execute | Must implement |
| `workflow_runtime` | Background work, planning, review, scheduling | Should implement |
| `host_admin_ux` | CLI convenience: `/help`, `/theme`, `/vim`, `/login`, `/logout` | Do not implement |
| `platform_specific` | Desktop/web/mobile-only surfaces | `unsupported_explicit` |
| `out_of_scope` | Organizational surfaces, internal-only hooks | Ignore |

This classification sits above tiers. A Tier 1 surface must also be `runtime_critical` or `workflow_runtime` to actually get implemented.

---

## Complete Claude Code Surface Inventory

Based on Claude Code tools reference and commands reference.

### Built-in Tools

| Tool | Family | Bucket | Tier |
|---|---|---|---|
| `Read` | file_code | runtime_critical | 0 |
| `Write` | file_code | runtime_critical | 0 |
| `Edit` | file_code | runtime_critical | 0 |
| `MultiEdit` | file_code | runtime_critical | 0 |
| `Glob` | file_code | runtime_critical | 0 |
| `Grep` | file_code | runtime_critical | 0 |
| `LS` | file_code | runtime_critical | 0 |
| `Bash` | execution | runtime_critical | 0 |
| `TaskCreate` | jobs | workflow_runtime | 1 |
| `TaskGet` | jobs | workflow_runtime | 1 |
| `TaskList` | jobs | workflow_runtime | 1 |
| `TaskUpdate` | jobs | workflow_runtime | 1 |
| `TaskStop` | jobs | workflow_runtime | 1 |
| `Agent` | subagents | workflow_runtime | 1 |
| `SendMessage` | subagents | workflow_runtime | 1 |
| `AskUserQuestion` | interaction | workflow_runtime | 1 |
| `EnterPlanMode` | planning | workflow_runtime | 2 |
| `ExitPlanMode` | planning | workflow_runtime | 2 |
| `EnterWorktree` | workspace | workflow_runtime | 2 |
| `ExitWorktree` | workspace | workflow_runtime | 2 |
| `CronCreate` | scheduling | workflow_runtime | 3 |
| `CronList` | scheduling | workflow_runtime | 3 |
| `CronDelete` | scheduling | workflow_runtime | 3 |
| `Monitor` | observability | workflow_runtime | 3 |
| `LSP` | code_intelligence | platform_specific | 3 |
| `ToolSearch` | meta | workflow_runtime | 3 |
| `WebFetch` | search_web | workflow_runtime | 3 |
| `WebSearch` | search_web | workflow_runtime | 3 |
| `NotebookRead` | notebook | workflow_runtime | 4 |
| `NotebookEdit` | notebook | workflow_runtime | 4 |
| `TodoWrite` | jobs | out_of_scope | 4 |
| `PowerShell` | execution | platform_specific | 5 |
| `TeamCreate` | teams | out_of_scope | 5 |
| `TeamDelete` | teams | out_of_scope | 5 |

Note: `TodoWrite` is only used in non-interactive mode / Agent SDK. In interactive sessions, Claude uses `TaskCreate/Get/List/Update/Stop`. The **task subsystem is high priority**.

**Availability gate notes for specific tools:**
- `Monitor` requires Claude Code v2.1.98+, unavailable on Bedrock/Vertex/Foundry, disabled by certain env vars. Its `availability_gate` must reflect this.
- `LSP` depends on code intelligence plugin + local language server binary. Its `availability_gate.required_plugins` and `required_binaries` must be populated.
- `SendMessage`, `TeamCreate`, `TeamDelete` require `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`. Their `availability_gate.env_flags` must include this.

### Built-in Commands

| Command | Family | Bucket | Tier |
|---|---|---|---|
| `/tasks` | jobs | workflow_runtime | 1 |
| `/security-review` | review | workflow_runtime | 1 |
| `/sandbox` | config_permissions | runtime_critical | 1 |
| `/plan` | planning | workflow_runtime | 2 |
| `/resume` | workspace | workflow_runtime | 2 |
| `/rewind` | workspace | workflow_runtime | 2 |
| `/permissions` | config_permissions | workflow_runtime | 2 |
| `/schedule` | durable_routines | workflow_runtime | 3 |
| `/init` | guidance_memory | workflow_runtime | 4 |
| `/memory` | guidance_memory | workflow_runtime | 4 |
| `/mcp` | mcp | workflow_runtime | 4 |
| `/plugin` | skills | workflow_runtime | 4 |
| `/doctor` | ui_misc | host_admin_ux | — |
| `/remote-control` | ui_misc | platform_specific | — |
| `/teleport` | ui_misc | platform_specific | — |
| `/desktop` | ui_misc | platform_specific | — |
| `/help` | ui_misc | host_admin_ux | — |
| `/theme` | ui_misc | host_admin_ux | — |
| `/vim` | ui_misc | host_admin_ux | — |
| `/login` | ui_misc | host_admin_ux | — |
| `/logout` | ui_misc | host_admin_ux | — |

Note: `/sandbox` promoted to Tier 1 (runtime_critical) because `thread/start` already takes `approvalPolicy` + `sandbox`. It is part of thread lifecycle, not a late config.

Note: `/permissions` promoted to Tier 2 because approval profile mapping is needed before Tier 1 tasks/subagents can safely run.

### Workflow Surfaces

| Surface | Family | Bucket | Tier |
|---|---|---|---|
| `code_review` | review | workflow_runtime | 1 |
| `security_review` | review | workflow_runtime | 1 |
| `rescue_fix` | review | workflow_runtime | 1 |
| `review_status` | review | workflow_runtime | 1 |
| `review_cancel` | review | workflow_runtime | 1 |

---

## Tiered Coverage Model

### Tier 0 — Core Tool Loop

`Read`, `Write`, `Edit`, `MultiEdit`, `Glob`, `Grep`, `LS`, `Bash`

Strategy: `native` or `mediated_native`. 100% fixture coverage required.

### Tier 1 — Approval + Tasks + Subagents + Review

`/sandbox`, `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, `TaskStop`, `Agent`, `SendMessage`, `AskUserQuestion`, `/tasks`, `/security-review`, review family

**`/sandbox` is in Tier 1** because approval policy is thread-lifecycle, not feature.

**`AskUserQuestion` is NOT approval.** It maps to `UserInteractionBridge` (see below), not the Approval Bridge.

Strategy: `mediated_native` or `workflow_emulated`.

### Tier 2 — Planning + Workspace + Permissions

`EnterPlanMode`, `ExitPlanMode`, `EnterWorktree`, `ExitWorktree`, `/plan`, `/resume`, `/rewind`, `/permissions`

**Plan mode is `mediated_native`**, not `workflow_emulated` — app-server has `item/plan/delta`.

**`/rewind` prefers `thread/rollback`**, not thread re-creation.

**`/permissions` is Tier 2** because task/subagent/review flows in Tier 1 need approval profiles.

### Tier 3 — Scheduling + Intelligence + Web

`CronCreate`, `CronList`, `CronDelete`, `Monitor`, `ToolSearch`, `WebFetch`, `WebSearch`, `/schedule`

**`Cron*` and `/schedule` are separate surface families:**
- `Cron*` → `session_scheduling` (ephemeral, session-scoped)
- `/schedule` → `durable_routines` (cloud infra, research preview, v1 unsupported)

**`ToolSearch` is `mediated_native`**, not `native` — Claude's deferred tool loading has no direct Codex equivalent.

### Tier 4 — Guidance + Skills + MCP

`/init`, `/memory`, `/mcp`, `/plugin`, `NotebookRead`, `NotebookEdit`

### Tier 5 — Platform-specific / Explicitly Unsupported / Out of Scope

All `host_admin_ux`, `platform_specific`, and `out_of_scope` surfaces.

---

## User Interaction Bridge

`AskUserQuestion` is Claude's tool for clarifying requirements from the user. App-server has approval pause/resume. These are **related but not identical**.

```rust
pub enum UserInteractionKind {
    /// Model needs information to proceed (AskUserQuestion)
    ClarificationQuestion {
        question: String,
        context: Option<String>,
    },
    /// Runtime needs permission to proceed (approval flow)  
    ApprovalRequest {
        action_description: String,
        approval_policy: ApprovalPolicy,
    },
}

pub struct UserInteractionBridge {
    pub interaction_id: String,
    pub kind: UserInteractionKind,
    pub turn_id: String,
    pub surface_id: String,
    pub status: InteractionStatus, // Pending | Answered | TimedOut | Cancelled
}
```

**Rules:**
- `AskUserQuestion` → `ClarificationQuestion` — pause turn, surface question to user, resume with answer
- Codex approval events → `ApprovalRequest` — surface approval request, resume with allow/deny
- Plan mode uses `ClarificationQuestion` to gather requirements before proposing
- Review/rescue human-in-the-loop uses `ClarificationQuestion`, not `ApprovalRequest`
- Never merge these two kinds; they have different UX, timeout, and retry semantics

---

## Planning: `mediated_native`, not `workflow_emulated`

V3 classified plan mode as `workflow_emulated`. This was too conservative.

App-server exposes:
- `item/plan/delta` — plan items in event stream
- `thread/rollback` — undo turns
- `thread/fork` — branch for exploratory work
- `thread/compact/start` — context window management

This means:

| Claude Surface | v3 Strategy | **v4 Strategy** | Codex Primitive |
|---|---|---|---|
| `EnterPlanMode` | `workflow_emulated` | **`mediated_native`** | instruction/profile switch + `item/plan/delta` awareness |
| `ExitPlanMode` | `workflow_emulated` | **`mediated_native`** | instruction/profile switch back |
| `/plan` | `workflow_emulated` | **`mediated_native`** | instruction injection + plan item surfacing |
| `/rewind` | `workflow_emulated` | **`mediated_native`** | `thread/rollback` (preferred) |
| rescue/review branch | — | **`mediated_native`** | `thread/fork` |

Future: `/compact` can map to `thread/compact/start`.

---

## Scheduling: Semantic Mismatch Policy

Claude has **two distinct scheduling surfaces** with different semantics:

1. **`CronCreate/CronList/CronDelete` and `/loop`** — Session-scoped scheduling. These live within the current session and vanish when Claude exits.
2. **`/schedule`** — Durable routines. Claude docs describe this as creating/updating/listing/running **routines** on Anthropic-managed cloud infrastructure, with triggers including scheduled, API, and GitHub. This is a research preview and fundamentally different from session cron.

These must NOT be treated as the same surface family.

```rust
pub enum SchedulingSurface {
    /// CronCreate/CronList/CronDelete, /loop — session-scoped, ephemeral
    SessionCron,
    /// /schedule — durable routines on cloud infrastructure
    DurableRoutine,
}
```

### Session Cron Policy

```rust
pub enum SchedulerMode {
    /// Claude semantics: session-scoped, ephemeral
    SessionCron {
        /// Cron entry disappears when proxy/session stops
        session_id: String,
    },
    /// Codex semantics: durable, persisted  
    DurableAutomation {
        /// Maps to Codex automation; outlives session
        /// Must emit warning that semantics changed
        automation_id: String,
    },
}
```

**Rules:**
- Default: `SessionCron` (preserves Claude semantics)
- If user explicitly requests persistent scheduling, upgrade to `DurableAutomation` with structured warning

### Durable Routine Policy

`/schedule` maps to a **different backend target** than `Cron*`:
- V1: `unsupported_explicit` or `workflow_emulated` — no direct Codex equivalent for Anthropic cloud routines
- Future: may map to Codex automations or a dedicated SDK backend

```rust
pub enum AutomationBackend {
    /// App-server for session-scoped work
    AppServerLocal,
    /// Responses API for simple fallback
    ResponsesFallback,
    /// Future: Codex SDK for CI-like jobs and durable automation
    FutureSdkBackend,
}
```

App-server docs note: for automating jobs or running Codex in CI, use the Codex SDK rather than app-server. This reinforces keeping a slot for a future SDK backend adapter.

---

## Surface Model

### `SurfaceDescriptor`

```rust
pub struct SurfaceDescriptor {
    pub id: String,
    pub source_provider: String,          // "claude_code"
    pub source_name: String,
    pub surface_kind: SurfaceKind,        // Tool | Command | Skill | Workflow | StateSurface | HostIntegration
    pub family: SurfaceFamily,
    pub bucket: SurfaceBucket,            // RuntimeCritical | WorkflowRuntime | HostAdminUx | PlatformSpecific | OutOfScope
    pub invocation_mode: InvocationMode,  // ModelInvoked | UserCommand | Implicit | Background
    pub state_scope: StateScope,          // Stateless | Request | Turn | Thread | Workspace | ProjectConfig | UserConfig | Job
    pub side_effect_level: SideEffectLevel, // None | LocalWrite | ShellExec | Network | StateMutation
    pub async_mode: AsyncMode,            // Sync | Async | Background
    pub approval_sensitivity: ApprovalSensitivity, // None | Ask | Strict
    pub host_dependency: HostDependency,  // None | LocalFs | Cli | Mcp | App | PlatformSpecific
    pub tier: u8,                         // 0-5
    pub availability_gate: AvailabilityGate,
}

/// Pre-conditions for a surface to be available at runtime.
/// Used by `doctor --json` to explain WHY a surface is unavailable.
pub struct AvailabilityGate {
    /// Minimum Claude Code / Codex version required
    pub min_version: Option<String>,
    /// Environment flags that must be set (e.g. CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1)
    pub env_flags: Vec<String>,
    /// Plugins that must be installed (e.g. code intelligence plugin for LSP)
    pub required_plugins: Vec<String>,
    /// External binaries that must be in PATH (e.g. language servers)
    pub required_binaries: Vec<String>,
    /// Platform constraint (e.g. "windows" for PowerShell)
    pub platform: Option<String>,
    /// Plan/product gate (e.g. "not available on Bedrock/Vertex/Foundry")
    pub plan_or_product: Option<String>,
    /// Whether the surface is behind an experimental flag
    pub experimental: bool,
}
```

### `MappingDecision`

```rust
pub struct MappingDecision {
    pub surface_id: String,
    pub target_backend: String,           // "codex_app_server" | "codex_responses_api"
    pub target_surface: Option<String>,
    pub strategy: MappingStrategy,        // Native | MediatedNative | WorkflowEmulated | UnsupportedExplicit
    pub fallback_mode: FallbackMode,      // HardError | SoftWarningAndContinue | DowngradeToWorkflow | DropWithObservability
    pub requires_mode: OperationMode,     // StrictAppServer | AutoHybrid | ResponsesOnly
    pub unsupported_reason: Option<UnsupportedReason>,
    pub warnings: Vec<String>,
}
```

### App-server-native State Types

```rust
/// Maps 1:1 to app-server Thread primitive
pub struct BridgeThread {
    pub thread_id: String,
    pub bridge_session_id: String,
    pub cwd: String,
    pub project_root: Option<String>,
    pub approval_policy: ApprovalPolicy,  // Set at thread/start
    pub sandbox_config: SandboxConfig,    // Set at thread/start
    pub created_at: Instant,
    pub turn_count: u64,
}

/// Maps 1:1 to app-server Turn primitive
pub struct BridgeTurn {
    pub turn_id: String,
    pub thread_id: String,
    pub role: TurnRole,                   // User | Assistant
    pub status: TurnStatus,              // Active | PausedForApproval | PausedForClarification | Completed | Failed
    pub items: Vec<BridgeItemRef>,
    pub pending_interaction: Option<UserInteractionBridge>,
}

/// Index into turn items for tracking
pub struct BridgeItemRef {
    pub item_id: String,
    pub item_type: ItemType,             // Text | ToolCall | ToolResult | ApprovalRequest | PlanDelta | ...
    pub surface_id: Option<String>,
}

/// High-level session wrapping thread + transport + config
pub struct BridgeSession {
    pub bridge_session_id: String,
    pub claude_session_id: Option<String>,
    pub thread: BridgeThread,
    pub transport: Transport,            // Stdio | Websocket
    pub operation_mode: OperationMode,
    pub api_stability: ApiStability,     // Stable | Experimental
    pub delegation_policy: DelegationPolicy,
    pub active_guidance_layers: Vec<String>,
    pub active_skills: Vec<String>,
    pub active_jobs: Vec<String>,
    pub state_version: u64,
}

/// Background work tracking
pub struct JobRecord {
    pub job_id: String,
    pub origin_surface_id: String,
    pub kind: JobKind,                   // Review | Rescue | Task | Schedule | Automation | Subagent | SessionCron | DurableAutomation
    pub status: JobStatus,               // Queued | Running | WaitingApproval | WaitingClarification | Completed | Failed | Cancelled
    pub scheduler_mode: Option<SchedulerMode>,
    pub codex_thread_id: Option<String>,
    pub codex_agent_ids: Vec<String>,
    pub worktree_path: Option<String>,
    pub result_summary: Option<String>,
    pub warnings: Vec<String>,
}
```

---

## Compatibility Matrix

### Tier 0 — Core Tools

| Claude Surface | Strategy | Codex Target | Notes |
|---|---|---|---|
| `Read` | `native` | file read | Direct parity |
| `Write` | `mediated_native` | file write | Preserve overwrite semantics, approval boundary |
| `Edit` | `mediated_native` | edit | Confirm patch granularity, protected paths |
| `MultiEdit` | `mediated_native` | repeated edit | Warn if atomicity not guaranteed |
| `Glob` | `native` | glob/file listing | Direct parity |
| `Grep` | `native` | grep/search | Direct parity |
| `LS` | `native` | directory listing | Direct parity |
| `Bash` | `mediated_native` | shell execution | Approval Bridge required; cwd/session continuity |

### Tier 1 — Approval + Tasks + Subagents + Review

| Claude Surface | Strategy | Codex Target | Notes |
|---|---|---|---|
| `/sandbox` | `mediated_native` | `thread/start` sandbox + approvalPolicy | Thread-lifecycle; must be Phase 1 |
| `TaskCreate` | `mediated_native` | thread/subagent spawn | Map to Codex task/thread creation |
| `TaskGet` | `mediated_native` | thread/job query | Map to job registry + thread state |
| `TaskList` | `mediated_native` | job registry list | Reflect active jobs/threads |
| `TaskUpdate` | `mediated_native` | turn injection | Map to thread message/instruction update |
| `TaskStop` | `mediated_native` | thread/job cancel | Cancel with structured result |
| `Agent` | `mediated_native` | subagent spawn | `DelegationPolicy` enforced; Codex explicit-spawn only |
| `SendMessage` | `mediated_native` | subagent message | Inter-agent communication via thread |
| `AskUserQuestion` | `mediated_native` | `UserInteractionBridge` | **Not approval.** Clarification question; pause/resume turn |
| `/tasks` | `mediated_native` | job registry list | UI for job/task listing |
| `/security-review` | `workflow_emulated` | agent + security spec | Security-biased review workflow |
| `code_review` | `workflow_emulated` | local code review agent | Findings, refs, severity, changes |
| `rescue_fix` | `workflow_emulated` | subagent/job spawn | Creates JobRecord; may use `thread/fork` |

### Tier 2 — Planning + Workspace + Permissions

| Claude Surface | Strategy | Codex Target | Notes |
|---|---|---|---|
| `EnterPlanMode` | **`mediated_native`** | instruction switch + `item/plan/delta` | App-server has plan items |
| `ExitPlanMode` | **`mediated_native`** | instruction switch back | Return to execution mode |
| `/plan` | **`mediated_native`** | instruction injection + plan item surfacing | Not just prompt template |
| `EnterWorktree` | `mediated_native` | **hybrid orchestration** | Thread state: native via app-server. Git worktree lifecycle: bridge-orchestrated via local git/Bash. Worktree↔thread association: bridge state. App-server does NOT have a dedicated worktree API. |
| `ExitWorktree` | `mediated_native` | **hybrid orchestration** | Cleanup git worktree (bridge-orchestrated) + return thread to main. |
| `/resume` | `mediated_native` | `thread/resume` | Resume paused thread |
| `/rewind` | **`mediated_native`** | **`thread/rollback`** | Prefer rollback over re-creation |
| `/permissions` | `mediated_native` | approval profile bridge | Needed before Tier 1 tasks can safely run |
| `/sandbox` | (already Tier 1) | | |

### Tier 3 — Scheduling + Intelligence + Web

| Claude Surface | Strategy | Codex Target | Notes |
|---|---|---|---|
| `CronCreate` | `mediated_native` | `SessionCron` (default) or `DurableAutomation` | **See scheduling mismatch policy** |
| `CronList` | `mediated_native` | scheduler registry query | Session-scoped or durable |
| `CronDelete` | `mediated_native` | scheduler entry removal | |
| `Monitor` | `workflow_emulated` | log/event watch | Emulate with polling or event subscription |
| `ToolSearch` | `mediated_native` | capability handshake + local deferred surface registry | Not true parity; only partial discovery/loading emulation. Claude ToolSearch does deferred tool loading, not just listing. |
| `WebFetch` | `native` or `mediated_native` | web/fetch | Depends on Codex runtime |
| `WebSearch` | `mediated_native` | search tool | Align filtering behavior |
| `/schedule` | `unsupported_explicit` | no Codex equivalent for Anthropic cloud routines | **Separate from `Cron*`.** Durable routine on cloud infra; v1 unsupported, future SDK backend |

### Tier 4 — Guidance + Skills + MCP

| Claude Surface | Strategy | Codex Target | Notes |
|---|---|---|---|
| `/init` | `workflow_emulated` | `AGENTS.md` bootstrap | Inspect repo → propose guidance |
| `/memory` | `workflow_emulated` | guidance proposal | Import from `CLAUDE.md`, proposal-first, no auto-sync |
| `/mcp` | `mediated_native` | MCP via `config.toml` | Codex CLI/IDE share config |
| `/plugin` | `workflow_emulated` | skill install | Normalize to Codex skill model |
| `NotebookRead` | `mediated_native` | document read | Verify format support |
| `NotebookEdit` | `unsupported_explicit` | — | Cell-level parity unlikely |

### Tier 5 — Explicitly Unsupported / Out of Scope

| Claude Surface | Bucket | Reason |
|---|---|---|
| `/doctor` | host_admin_ux | CLI convenience |
| `/help` | host_admin_ux | CLI convenience |
| `/theme` | host_admin_ux | CLI convenience |
| `/vim` | host_admin_ux | CLI convenience |
| `/login` / `/logout` | host_admin_ux | CLI convenience |
| `/remote-control` | platform_specific | Desktop/web-only |
| `/teleport` | platform_specific | Desktop/web-only |
| `/desktop` | platform_specific | Desktop/web-only |
| `PowerShell` (non-Windows) | platform_specific | OS-dependent |
| `LSP` | platform_specific | Needs local language server |
| `TeamCreate` / `TeamDelete` | out_of_scope | Organizational |
| `TodoWrite` (interactive) | out_of_scope | Replaced by `Task*` |

---

## Runtime Architecture

### Request Flow

```
Claude-compatible client
  → Ingress Adapter (parse tool calls, commands, skill mentions, runtime intent)
  → Surface Classifier (→ SurfaceDescriptor + MappingDecision)
  → Session Manager (BridgeSession, BridgeThread lifecycle)
  → User Interaction Bridge (clarification questions)
  → Approval Bridge (approval policy, pause/resume)
  → App-server Runtime Adapter (JSON-RPC over stdio)
      → initialize / initialized handshake
      → thread/start (with approvalPolicy + sandbox)
      → turn/start
      → streamed Item events (including item/plan/delta)
      → approval request/response
      → thread/rollback, thread/fork, thread/compact
  → Event Translator (Codex items → Claude-compatible events)
  → State + Job Registry (threads, turns, items, jobs, scheduler)
  → Response Synthesizer (+ bridge metadata)
  → Claude-compatible output
```

### Component Details

**A. Ingress Adapter** — Extends `src/routes/mod.rs`. Existing `handle_anthropic_messages()` becomes one path; surface-aware dispatch added alongside.

**B. Surface Classifier** — Routes to correct family handler. Classifies by bucket + tier. Delegates skill mentions to existing `src/skills/`.

**C. Session Manager** — **New.** Manages `BridgeSession` wrapping `BridgeThread`. Handles thread lifecycle, resume/reconnect, model overrides. Sets `approvalPolicy` and `sandbox` at `thread/start`.

**D. User Interaction Bridge** — **New.** Handles `AskUserQuestion` (clarification) separately from approval. Pause/resume turns with typed interaction kinds.

**E. Approval Bridge** — **First-class, Phase 1.** Codex defaults: network off, actions outside sandbox need approval. Maps Claude permission model. All shell/write/network flows go through this. Handles server-initiated approval pause/resume on turns. Sets `approvalPolicy` at thread creation.

**F. App-server Runtime Adapter** — JSON-RPC client. Spawns `codex app-server` as child process (stdio). Handles: `initialize`/`initialized` handshake (with `apiStability` flag), `thread/start`, `turn/start`, streamed item events, approval request/response, `turn/cancel`, `thread/rollback`, `thread/fork`, `thread/compact/start`, model discovery.

**G. Event Translator** — Extends patterns from `src/proxy/event_extractor.rs` and `src/translation/codex_to_anthropic.rs`. Maps: assistant output chunks, approval requests, tool-use status, **plan items** (`item/plan/delta`), background job references, subagent events, downgrade warnings.

**H. State + Job Registry** — Tracks: `BridgeThread`/`BridgeTurn`/`BridgeItemRef` (app-server-native), `JobRecord` for background work, `SchedulerMode` for cron entries, guidance mapping, review runs, worktree associations.

**I. Skill Bridge** — Existing `src/skills/`. Unchanged scope.

**J. Guidance Bridge** — `CLAUDE.md`/memory → `AGENTS.md`/`.codex/config.toml`. V1: read-only import, proposal-first, no auto-sync.

---

## Bootstrap & Product Track

### `claude-codex-proxy setup`

1. Check `codex` binary exists and is in PATH
2. Check `codex login` auth state (not `auth.json`)
3. Try `codex app-server` spawn + JSON-RPC handshake
4. Verify model discovery via handshake
5. Call `configRequirements/read` to discover `allowedApprovalPolicies`, `allowedSandboxModes`, and feature requirements
6. Smoke test via `command/exec` (e.g. `["pwd"]`) to confirm sandbox path works without needing a full thread
7. Test permission/approval round-trip
8. Print Claude-compatible endpoint config snippet

**`setup --write-config`** — Automatically write config snippet to common Claude client config locations.

### `claude-codex-proxy doctor`

Report:
- Transport mode (app-server stdio / websocket / responses-only)
- `codex` binary version
- API stability (stable / experimental)
- Supported surface tiers (based on capability handshake)
- `configRequirements/read` results: allowed approval policies, sandbox modes, feature requirements
- Degraded surfaces with reasons (including `AvailabilityGate` check results)
- Auth state
- Operation mode
- Delegation policy

**`doctor --json`** — Machine-readable output for scripts/UI.

### `claude-codex-proxy env`

Output ready-to-use config for Claude client:

```json
{
  "ANTHROPIC_API_KEY": "dummy",
  "ANTHROPIC_BASE_URL": "http://127.0.0.1:8080",
  "ANTHROPIC_MODEL": "<discovered-model>"
}
```

**`env --shell bash|zsh|fish|powershell`** — Output in correct shell export format.

### README policy

**README must not claim app-server-first until Phase 1 passes end-to-end.** Current README accurately describes Responses API proxy. Update README only when code matches.

---

## Proposed Directory Structure

```
src/
  app_server/
    mod.rs
    client.rs              — JSON-RPC client, spawn codex app-server
    jsonrpc.rs             — JSON-RPC 2.0 message types and framing
    transport_stdio.rs     — stdio transport (primary)
    transport_ws.rs        — websocket transport (secondary)
    session.rs             — BridgeSession lifecycle
    thread.rs              — BridgeThread / BridgeTurn / BridgeItemRef
    events.rs              — Raw event types from app-server
    handshake.rs           — initialize / initialized / capability discovery
    schema_stable.rs       — Generated types from stable API surface
    schema_experimental.rs — Generated types from experimental API surface

  surfaces/
    mod.rs
    model.rs               — SurfaceDescriptor, MappingDecision, SurfaceBucket, all enums
    classifier.rs          — Classify incoming requests into surfaces
    registry.rs            — Static surface registry (all known surfaces + tiers + buckets)
    matrix.rs              — Compatibility matrix lookup

  mapping/
    mod.rs
    decision.rs            — MappingDecision resolution
    tools.rs               — Tool surface mapping (Tier 0)
    tasks.rs               — Task family mapping (Tier 1)
    subagents.rs           — Agent/SendMessage + DelegationPolicy (Tier 1)
    review.rs              — Review family mapping (Tier 1)
    planning.rs            — Plan mode mapping via item/plan/delta (Tier 2)
    workspace.rs           — Worktree/resume/rewind/rollback/fork (Tier 2)
    scheduling.rs          — SessionCron + DurableAutomation (Tier 3)
    commands.rs            — Command surface mapping
    workflows.rs           — Workflow surface mapping
    guidance.rs            — Guidance/memory bridge
    skills.rs              — Delegates to src/skills/
    approvals.rs           — Approval policy translation (Phase 1)
    interaction.rs         — UserInteractionBridge (clarification vs approval)

  jobs/
    mod.rs
    model.rs               — JobRecord, JobKind, JobStatus, SchedulerMode
    registry.rs            — In-memory job tracking
    review.rs              — Review workflow jobs
    rescue.rs              — Rescue/fix workflow jobs
    task.rs                — Task lifecycle (TaskCreate/Get/List/Update/Stop)
    scheduler.rs           — SessionCron + DurableAutomation management

  state/
    mod.rs
    store.rs               — Session state persistence
    guidance.rs            — Guidance layer state
    permissions.rs         — Approval profile state

  adapters/
    anthropic_ingress.rs   — Claude-format request parsing
    claude_output.rs       — Claude-format response synthesis

  observability/
    mod.rs
    logs.rs                — Structured logging
    traces.rs              — Request tracing with surface context
    diagnostics.rs         — Bridge diagnostic endpoint support

  cli/
    mod.rs
    setup.rs               — claude-codex-proxy setup [--write-config]
    doctor.rs              — claude-codex-proxy doctor [--json]
    env.rs                 — claude-codex-proxy env [--shell ...]

  # Existing modules preserved:
  domain/                  — Protocol types (unchanged)
  proxy/                   — Responses API client (fallback)
  routes/                  — HTTP routes (extended)
  skills/                  — Custom skill bridge (unchanged)
  translation/             — Protocol translation (Responses fallback path)
```

---

## Impact on Current Codebase

### Preserved as-is
- `src/domain/` — All protocol types remain valid
- `src/skills/` — Custom skill bridge stays separate
- `src/translation/` — Protocol translation for Responses fallback
- `src/proxy/event_extractor.rs` — SSE parsing patterns reusable
- `src/proxy/sse_parser.rs` — SSE framing reusable

### Extended
- `src/routes/mod.rs` — Surface-aware dispatch alongside existing handlers
- `src/main.rs` — App-server spawn, session management, CLI subcommands, operation mode + API stability selection

### Becomes fallback
- `src/proxy/codex_client.rs` — Used only in `auto-hybrid` (degraded) and `responses-only` modes
- `--auth-path` — Deprecated for app-server mode; kept for `responses-only`

### New
- `src/app_server/` — JSON-RPC client, transports, Thread/Turn/Item state, handshake, schema
- `src/surfaces/` — Surface model, classifier, registry, matrix, buckets
- `src/mapping/` — Surface-to-Codex mapping (tiered), interaction bridge, delegation policy
- `src/jobs/` — Background job + task tracking, scheduler modes
- `src/state/` — Session state management
- `src/adapters/` — Ingress/egress format adapters
- `src/observability/` — Bridge diagnostics
- `src/cli/` — setup/doctor/env subcommands

---

## Fallback Policy

Every surface must declare one fallback mode:

| Mode | When |
|---|---|
| `hard_error` | Critical side effects that cannot safely degrade |
| `soft_warning_and_continue` | Can proceed with reduced functionality |
| `downgrade_to_workflow` | Emulated via instruction when no runtime primitive |
| `drop_with_observability` | Dropped entirely, logged for diagnostics |

**Rules:**
- Runtime tools with side effects must not silently degrade to prompt text
- Workflow surfaces may degrade to instructions if behavior remains predictable
- Stateful features must declare whether state is preserved, approximated, or lost
- Operation mode determines which fallbacks are available
- `host_admin_ux` and `out_of_scope` surfaces → `drop_with_observability`

---

## Observability

### Response Metadata

```json
{
  "bridge": {
    "surface_id": "tool.task_create",
    "strategy": "mediated_native",
    "target_backend": "codex_app_server",
    "operation_mode": "auto-hybrid",
    "api_stability": "stable",
    "downgraded": false,
    "tier": 1,
    "bucket": "workflow_runtime",
    "warnings": []
  }
}
```

### Diagnostics Endpoints

| Endpoint | Purpose |
|---|---|
| `GET /bridge/surfaces` | All known surfaces with mapping decisions, buckets, tiers |
| `GET /bridge/surfaces/:id` | Detail for specific surface |
| `GET /bridge/jobs` | Active job registry |
| `GET /bridge/compatibility` | Full compatibility matrix |
| `GET /bridge/session/:id` | Session/thread state inspection |
| `GET /bridge/mode` | Operation mode + API stability + delegation policy + degradation |

---

## Phased Rollout

### Phase -1 — Bootstrap & Doctor

**Deliverables:**
- `src/cli/setup.rs` — `claude-codex-proxy setup [--write-config]`
- `src/cli/doctor.rs` — `claude-codex-proxy doctor [--json]`
- `src/cli/env.rs` — `claude-codex-proxy env [--shell ...]`
- Detect `codex` binary, auth state, app-server spawn capability
- Print/write config snippet for Claude clients

**Exit criteria:**
- From clean machine with `codex` installed: setup completes in ≤ 3 steps
- `doctor` reports transport, version, auth, API stability, surface tiers
- `doctor --json` outputs machine-readable profile
- `env` outputs usable Claude client config in requested shell format

### Phase 0 — Surface Model + Matrix

**Deliverables:**
- `docs/surfaces/claude-inventory.md` — Complete Claude surface inventory with buckets
- `docs/surfaces/codex-target-inventory.md` — Codex app-server surface inventory
- `docs/surfaces/compatibility-matrix.md` — Full matrix with tiers + buckets
- `src/surfaces/model.rs` — `SurfaceDescriptor`, `MappingDecision`, `SurfaceBucket`, all enums
- `src/surfaces/matrix.rs` — Matrix lookup

**Exit criteria:**
- Every Claude surface classified with tier, bucket, strategy, fallback
- `host_admin_ux` / `out_of_scope` surfaces explicitly excluded from implementation backlog
- Surface model compiles and is unit-tested

### Phase 1 — App-server Foundation + Approval

**Deliverables:**
- `src/app_server/client.rs` — Spawn `codex app-server`, JSON-RPC client
- `src/app_server/transport_stdio.rs` — stdio transport
- `src/app_server/handshake.rs` — `initialize` / `initialized` + capability discovery + `apiStability` flag
- `src/app_server/thread.rs` — `BridgeThread` / `BridgeTurn` / `BridgeItemRef`
- `src/app_server/session.rs` — `BridgeSession` with `DelegationPolicy`, `ApiStability`
- `src/app_server/events.rs` — Streamed event types
- `src/mapping/approvals.rs` — **Full Approval Bridge** (not skeleton): `approvalPolicy` + `sandbox` at `thread/start`, approval pause/resume
- `src/mapping/interaction.rs` — `UserInteractionBridge` with `ClarificationQuestion` / `ApprovalRequest`
- `/sandbox` surface mapping
- Operation mode selection (`strict-app-server` / `auto-hybrid` / `responses-only`)
- Stable vs experimental API flag

**Exit criteria:**
- Can spawn app-server, handshake, start thread (with approval policy + sandbox), start turn, receive streamed items
- Approval request pause/resume works end-to-end
- Clarification question pause/resume works (separate from approval)
- `/sandbox` config applied at thread creation
- `stdio` transport end-to-end
- `auto-hybrid` falls back to Responses when app-server unavailable
- Stable API default; experimental opt-in tested

### Phase 2 — Core Tool Parity + Permissions (Tier 0 + partial Tier 2)

**Deliverables:**
- `src/mapping/tools.rs` — Map `Read`/`Write`/`Edit`/`MultiEdit`/`Glob`/`Grep`/`LS`/`Bash`
- Path/cwd/session normalization
- Protected-path + approval tests
- `/permissions` — approval profile mapping (minimum viable)
- Golden fixtures for all 8 core tools

**Exit criteria:**
- 100% core tool fixtures pass
- Structured downgrade warnings
- Shell flow preserves session/thread continuity
- Approval Bridge enforced for `Write`/`Edit`/`Bash`
- Permission profiles loadable and applied to threads

### Phase 3 — Tasks + Subagents + Review (Tier 1)

**Deliverables:**
- `src/jobs/task.rs` — `TaskCreate`/`Get`/`List`/`Update`/`Stop` lifecycle
- `src/mapping/tasks.rs` — Task surface mapping
- `src/mapping/subagents.rs` — `Agent`/`SendMessage` mapping with `DelegationPolicy`
- `src/mapping/review.rs` — Review family (code_review, security_review, rescue_fix, status, cancel)
- `src/jobs/registry.rs` — Job tracking
- `AskUserQuestion` → `UserInteractionBridge.ClarificationQuestion`

**Exit criteria:**
- Task CRUD lifecycle works through app-server threads
- `DelegationPolicy.ExplicitOnly` enforced by default
- Subagent spawn and message passing works
- Review workflow end-to-end with status/result/cancel
- `rescue_fix` can use `thread/fork`
- Child/subagent approval doesn't break parent flow
- Clarification questions surface correctly in review/plan contexts

### Phase 4 — Planning + Workspace (Tier 2)

**Deliverables:**
- `src/mapping/planning.rs` — `EnterPlanMode`/`ExitPlanMode`/`/plan` via `mediated_native` (instruction switch + `item/plan/delta`)
- `src/mapping/workspace.rs` — `EnterWorktree`/`ExitWorktree`/`/resume`/`/rewind`
- `/rewind` → `thread/rollback` (preferred)
- `thread/fork` for branch work

**Exit criteria:**
- Plan mode switch via instruction profile + plan item awareness
- `item/plan/delta` events surfaced in Claude-compatible format
- Worktree enter/exit maps to Codex worktree support
- `/rewind` uses `thread/rollback`, not thread re-creation
- Session resume preserves thread state

### Phase 5 — Scheduling + Intelligence + Web (Tier 3)

**Deliverables:**
- `src/mapping/scheduling.rs` — `Cron*` with `SessionCron` / `DurableAutomation` modes
- `src/jobs/scheduler.rs` — Session-scoped scheduler
- `/schedule` → `unsupported_explicit` with structured reason (durable routine, no Codex equivalent yet)
- `WebFetch`/`WebSearch` mapping
- `Monitor` emulation
- `ToolSearch` → `mediated_native` via capability handshake + local deferred registry

**Exit criteria:**
- `SessionCron` preserves Claude ephemeral semantics
- `DurableAutomation` emits structured warning about semantic change
- `/schedule` returns structured unsupported reason, not silent failure
- Web tools work where Codex runtime exposes them
- `ToolSearch` clearly documented as partial emulation

### Phase 6 — Guidance + Skills + MCP (Tier 4)

**Deliverables:**
- `src/mapping/guidance.rs` — `/init` → `AGENTS.md`, `/memory` → import proposal
- `src/state/guidance.rs` — Guidance layer state
- `/mcp` → Codex `config.toml` MCP bridge
- Skill bridge integration (extends `src/skills/`)

**Exit criteria:**
- `AGENTS.md` creation/update reliable
- No auto-sync between Claude memory and Codex guidance
- MCP reads/writes through `config.toml`
- Simple skills compile and invoke through app-server

### Phase 7 — Hardening + Regression

**Deliverables:**
- Schema generation from pinned binary: `schema_stable.rs` + `schema_experimental.rs`
- Compatibility report endpoint (`GET /bridge/compatibility`)
- Telemetry for degradation tracking
- Full regression suite across all tiers
- README update (only after code matches)

**Exit criteria:**
- Schema types generated from `codex app-server generate-json-schema`
- All tier coverage targets met
- 0 silent downgrades on side-effect surfaces
- README accurately describes current architecture
- `doctor --json` output matches actual capability profile

---

## Testing Strategy

### Test Levels

| Level | Scope |
|---|---|
| Unit | Surface classification + bucket assignment, mapping decision, approval decision, interaction bridge dispatch, delegation policy enforcement, scheduler mode selection, guidance normalization, Thread/Turn/Item state |
| Integration | App-server transport, session lifecycle, streamed event translation, approval pause/resume, clarification pause/resume, thread/rollback, thread/fork |
| Conformance | Golden fixtures: Claude request → expected mapping decision + warnings per tier |
| Workflow | Task lifecycle, review job lifecycle, plan mode (with plan items), worktree, cron (both modes), init/memory/guidance flow |

### Priority Fixtures (by tier)

| Tier | Required Fixtures |
|---|---|
| 0 | `Read`, `Write`, `Edit`, `MultiEdit`, `Bash`, `Glob`, `Grep`, `LS` |
| 1 | `/sandbox`, `TaskCreate`, `TaskGet`, `TaskList`, `Agent` (with delegation policy), `code_review`, `AskUserQuestion` (as clarification) |
| 2 | `EnterPlanMode` (with `item/plan/delta`), `EnterWorktree`, `/resume`, `/rewind` (via `thread/rollback`), `/permissions` |
| 3 | `CronCreate` (both `SessionCron` and `DurableAutomation`), `WebFetch` |
| 4 | `memory_import`, `init_guidance_bootstrap` |

### Acceptance Metrics

- 100% Tier 0 fixtures pass
- ≥ 80% Tier 1-2 fixtures pass
- 0 silent downgrades on surfaces with side effects
- 0 `AskUserQuestion` dispatched as approval (must be clarification)
- `DelegationPolicy.ExplicitOnly` enforced in all Tier 1 tests
- Session resume preserves thread/cwd/guidance/approval state
- Setup → first session ≤ 3 steps

---

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Mixing skill bridge with runtime bridge | Module boundaries locked: `surfaces/` + `app_server/` + `jobs/` before `skills/` expansion |
| Over-claiming parity | Only 4 strategy states; tier + bucket classification; no "best effort" |
| Task/subagent deferred too long | Task family is Tier 1; `/sandbox` + approval is Phase 1 |
| State model impedance mismatch | Thread/Turn/Item first, then bridge wrappers |
| Protocol drift | Pin binary; generate schema (stable/experimental split); capability handshake |
| Memory/guidance sync breaks repo | V1: proposal-first only; no auto-sync |
| Approval mismatch = unsafe | Approval Bridge is Phase 1 foundation; `thread/start` includes policy |
| `AskUserQuestion` conflated with approval | `UserInteractionBridge` separates clarification from approval |
| Subagent auto-delegation unpredictable | `DelegationPolicy.ExplicitOnly` default |
| Cron semantic mismatch | `SessionCron` vs `DurableAutomation` with explicit warnings; `/schedule` is separate unsupported surface |
| `/schedule` conflated with `Cron*` | Separate families: `session_scheduling` vs `durable_routines` |
| `ToolSearch` overclaimed as native | Downgraded to `mediated_native`; partial discovery emulation only |
| `EnterWorktree` implies full API | Documented as hybrid: thread state native, git worktree bridge-orchestrated |
| `codex app-server` instability | `auto-hybrid` fallback; `doctor` reports degradation; stable API default |
| Backlog drift to low-value surfaces | `SurfaceBucket` classification; `host_admin_ux`/`out_of_scope` never enter implementation queue |
| README ahead of code | README update gated on Phase 1 passing end-to-end |

---

## Key Differences from v3

| v3 | v4-final |
|---|---|
| No surface bucket classification | **`SurfaceBucket`**: runtime_critical / workflow_runtime / host_admin_ux / platform_specific / out_of_scope |
| No availability gate | **`AvailabilityGate`**: min_version, env_flags, required_plugins, required_binaries, platform, plan_or_product, experimental |
| Approval Bridge as Phase 1 "skeleton" | **Full Approval Bridge in Phase 1**; `/sandbox` is Tier 1; `/permissions` is Tier 2 |
| `AskUserQuestion` mapped to approval | **`UserInteractionBridge`** separates `ClarificationQuestion` from `ApprovalRequest` |
| Plan mode `workflow_emulated` | **`mediated_native`** via `item/plan/delta`, `thread/rollback`, `thread/fork` |
| `/rewind` as thread re-creation | **`thread/rollback`** preferred |
| No branch primitive | **`thread/fork`** for rescue/review/branch work |
| `Cron*` and `/schedule` in same family | **`SessionCron`** (ephemeral) vs **`DurableRoutine`** (`/schedule`, cloud infra, separate surface) |
| No delegation policy | **`DelegationPolicy`** enum; default `ExplicitOnly` |
| No API stability flag | **`--app-server-stable`** (default) / **`--app-server-experimental`**; schema split |
| `ToolSearch` as `native` | **`mediated_native`** — partial discovery/loading emulation, not true deferred tool loading parity |
| `EnterWorktree` implied full API | **Hybrid orchestration**: thread state native, git worktree bridge-orchestrated, no dedicated app-server worktree API |
| `setup`/`doctor`/`env` basic | **`setup --write-config`**, **`doctor --json`**, **`env --shell ...`** |
| README updated freely | **README gated on Phase 1 end-to-end pass** |

---

## Execution Order Summary

1. **Bootstrap/doctor/setup** (Phase -1)
2. **Surface model + matrix + buckets** (Phase 0)
3. **App-server stdio + handshake + Thread/Turn/Item + full Approval Bridge + `/sandbox` + UserInteractionBridge + stable/experimental API** (Phase 1)
4. **Core tool parity + `/permissions`** (Phase 2)
5. **Task + subagent (with DelegationPolicy) + review** (Phase 3)
6. **Plan (mediated_native) + worktree + rollback + fork** (Phase 4)
7. **Scheduling (dual-mode) + web + intelligence** (Phase 5)
8. **Guidance/init/memory/skills/MCP** (Phase 6)
9. **Hardening + schema generation + regression + README update** (Phase 7)

Approval is foundation. Tasks/subagents before memory/skills. Plan mode is native, not emulated. `Cron*` and `/schedule` are separate families. Delegation is explicit. Worktree is hybrid orchestration. README follows code.

---

## One-liner

> `claude-codex-proxy` is an app-server-first Claude-compatible Codex runtime host that maps Claude Code surfaces—tools, commands, workflows, guidance, and jobs—onto Codex runtime semantics with native execution where possible, transparent degradation where not, and one-command setup.

