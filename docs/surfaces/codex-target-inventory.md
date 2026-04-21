# Codex App-Server Target Inventory

The bridge targets the current `codex app-server` JSON-RPC surface exposed by `codex-cli 0.104.0`.

## Core Requests

| Method | Purpose |
|---|---|
| `initialize` / `initialized` | Capability negotiation and lifecycle start |
| `configRequirements/read` | Discover allowed approval, sandbox, and web search policies |
| `model/list` | Discover available models |
| `command/exec` | One-off sandboxed command execution |
| `thread/start`, `thread/resume`, `thread/fork`, `thread/rollback`, `thread/compact/start` | Thread lifecycle |
| `turn/start`, `turn/interrupt`, `turn/steer` | Turn lifecycle |
| `account/read` / `getAuthStatus` | Auth status and account information |
| `review/start` | Review workflow entrypoint |
| `skills/list`, `skills/config/write`, `skills/remote/read`, `skills/remote/write` | Skill management surfaces |

## Key Notifications

| Method | Purpose |
|---|---|
| `thread/started`, `turn/started`, `turn/completed` | Thread and turn lifecycle |
| `item/started`, `item/completed`, `item/agentMessage/delta` | Item lifecycle and assistant streaming |
| `item/plan/delta`, `turn/plan/updated` | Plan-mode output |
| `error` | Retryable and terminal transport/runtime errors |
| `terminal_interaction` | User interaction pauses and prompts |
| `command/execution/output/delta`, `file/change/output/delta` | Side-effect streaming |

## Runtime Notes

- `thread/start` accepts `approvalPolicy` and `sandbox`.
- `turn/start` accepts per-turn `approvalPolicy`, `cwd`, `model`, `personality`, `sandboxPolicy`, and `outputSchema`.
- `model/list` falls back to cached models when the upstream backend is unavailable.
- The server emits extra `codex/event/*` notifications in parallel with the stable notification names.
