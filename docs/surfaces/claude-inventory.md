# Claude Surface Inventory

This inventory mirrors the frozen bridge plan and groups each Claude Code surface by kind, family, bucket, and tier.

## Tools

| Surface | Family | Bucket | Tier |
|---|---|---|---|
| `Read`, `Write`, `Edit`, `MultiEdit`, `Glob`, `Grep`, `LS` | `file_code` | `runtime_critical` | `0` |
| `Bash` | `execution` | `runtime_critical` | `0` |
| `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, `TaskStop` | `jobs` | `workflow_runtime` | `1` |
| `Agent`, `SendMessage` | `subagents` | `workflow_runtime` | `1` |
| `AskUserQuestion` | `interaction` | `workflow_runtime` | `1` |
| `EnterPlanMode`, `ExitPlanMode` | `planning` | `workflow_runtime` | `2` |
| `EnterWorktree`, `ExitWorktree` | `workspace` | `workflow_runtime` | `2` |
| `CronCreate`, `CronList`, `CronDelete` | `scheduling` | `workflow_runtime` | `3` |
| `Monitor`, `ToolSearch`, `WebFetch`, `WebSearch` | `observability`, `meta`, `search_web` | `workflow_runtime` | `3` |
| `NotebookRead`, `NotebookEdit` | `notebook` | `workflow_runtime` | `4` |
| `LSP`, `PowerShell` | `code_intelligence`, `execution` | `platform_specific` | `3-5` |
| `TodoWrite`, `TeamCreate`, `TeamDelete` | `jobs`, `teams` | `out_of_scope` | `4-5` |

## Commands

| Surface | Family | Bucket | Tier |
|---|---|---|---|
| `/tasks`, `/security-review` | `jobs`, `review` | `workflow_runtime` | `1` |
| `/sandbox` | `config_permissions` | `runtime_critical` | `1` |
| `/plan`, `/resume`, `/rewind`, `/permissions` | `planning`, `workspace`, `config_permissions` | `workflow_runtime` | `2` |
| `/schedule` | `durable_routines` | `workflow_runtime` | `3` |
| `/init`, `/memory`, `/mcp`, `/plugin` | `guidance_memory`, `mcp`, `skills` | `workflow_runtime` | `4` |
| `/doctor`, `/help`, `/theme`, `/vim`, `/login`, `/logout` | `ui_misc` | `host_admin_ux` | `—` |
| `/remote-control`, `/teleport`, `/desktop` | `ui_misc` | `platform_specific` | `—` |

## Workflows

| Surface | Family | Bucket | Tier |
|---|---|---|---|
| `code_review`, `security_review`, `rescue_fix`, `review_status`, `review_cancel` | `review` | `workflow_runtime` | `1` |
