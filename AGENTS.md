# AGENTS.md

> AI agent guide for the **claude-codex-proxy** codebase.

## What This Project Is

An app-server-first Rust proxy that maps Claude Code surfaces onto Codex runtime semantics. Bridges Anthropic Messages API and OpenAI Chat Completions API clients to Codex via app-server JSON-RPC (primary) or Responses API (fallback).

## Architecture

### Three operation modes
- `auto-hybrid` (default): app-server first, Responses API fallback
- `strict-app-server`: fail without app-server
- `responses-only`: legacy stateless translation

### Module layout
- `src/app_server/` — JSON-RPC client, stdio transport, Thread/Turn/Item state, handshake, schema
- `src/surfaces/` — Surface model, classifier, registry, compatibility matrix
- `src/jobs/` — JobExecutor (in-process app-server runtime), background job tracking, registry
- `src/state/` — Session state, guidance layers, permission profiles
- `src/adapters/` — Claude-format output synthesis with bridge metadata
- `src/observability/` — Degradation telemetry and structured tracing
- `src/cli/` — setup/doctor/env subcommands
- `src/domain/` — Serde types: anthropic.rs, openai.rs, codex.rs, auth.rs
- `src/proxy/` — Legacy Responses API client (fallback path)
- `src/routes/` — Warp HTTP handlers, rate-limiting guards, and DispatchPlanner
- `src/translation/` — Protocol bridging (Anthropic↔Codex↔OpenAI) with tool-call assembler
- `src/skills/` — Custom skill bridge (marker detection, instruction injection)

### Request Flow (app-server mode)
```
Client → Warp handler → DispatchPlanner (Surface Classifier)
  → App-server JSON-RPC via JobExecutor (isolated thread/turn handling)
  → Event Translator (ToolCallAssembler) → Claude / OpenAI output
```

### Key Design Decisions
1. App-server-first via stdio JSON-RPC; Responses API = fallback
2. Surface-first: every Claude surface has bucket (runtime_critical/workflow_runtime/host_admin_ux/platform_specific/out_of_scope) + tier (0-5)
3. State: BridgeThread > BridgeTurn > BridgeItemRef maps 1:1 to app-server primitives
4. Approval Bridge is Phase 1 foundation — approvalPolicy + sandbox set at thread/start
5. Task Completion MVP: The Proxy bypasses rate-limited Responses API by routing eligible traffic to the underlying AppServer with a dedicated `JobExecutor`.
6. Thread Leasing: Uses a `ThreadPool` and explicit continuation (TaskUpdate, /resume) instead of generic stateless thread reuse.
7. Plan mode = mediated_native via item/plan/delta + thread/rollback + thread/fork
8. DelegationPolicy.ExplicitOnly default for subagent spawning

## Build and Run

```bash
cargo build --release
./target/release/claude-codex-proxy setup        # check prerequisites
./target/release/claude-codex-proxy serve         # start server
./target/release/claude-codex-proxy doctor --json # diagnostics
```

Config: CLI arg > env var > default. Port 8080. Auth via `codex login` (app-server) or `~/.codex/auth.json` (responses-only).

## Testing

```bash
cargo test                    # all unit + integration tests (160+)
cargo test -- --ignored       # integration tests requiring live codex app-server
cargo test --tests --no-run   # Compile checks for integration tests
cargo clippy                  # lint
```

Key test modules: `mapping::tools::tests`, `mapping::approvals::tests`, `mapping::interaction::tests`, `mapping::tasks::tests`, `mapping::subagents::tests`, `mapping::review::tests`, `mapping::planning::tests`, `mapping::workspace::tests`, `mapping::scheduling::tests`. Integration regression: `tests/surface_bridge_regression.rs`.

## Key Files

- Surface model: `src/surfaces/model.rs` (SurfaceDescriptor, MappingDecision, all enums)
- Tool mapping: `src/mapping/tools.rs` (Read/Write/Edit/Bash/Glob/Grep/LS + web + notebook)
- Approval: `src/mapping/approvals.rs` (policy precedence, pause detection, sandbox intent)
- Interaction: `src/mapping/interaction.rs` (clarification vs approval classification)
- Tasks: `src/mapping/tasks.rs` + `src/jobs/` (TaskCreate/Get/List/Update/Stop)
- Planning: `src/mapping/planning.rs` (plan mode, plan delta events)
- Workspace: `src/mapping/workspace.rs` (worktree hybrid orchestration, resume, rewind/rollback)
- Schema: `src/app_server/schema_stable.rs`, `schema_experimental.rs`
- Architecture Docs (Local only): `docs/codex-task-completion-analysis.md` (quota hypotheses) and `docs/superpowers/plans/` (implementation plans)
