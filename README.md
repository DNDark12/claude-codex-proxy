# claude-codex-proxy

An **app-server-first** Claude-compatible Codex runtime host that maps Claude Code surfaces—tools, commands, workflows, guidance, and jobs—onto Codex runtime semantics with native execution where possible, transparent degradation where not, and one-command setup.

## Fastest Start

If you just want to run the proxy locally, **do not install it globally first**. Run it directly from the repo:

```bash
cargo run -- setup
cargo run -- serve
```

In another terminal, print the client config:

```bash
cargo run -- env --shell zsh
```

This is the simplest path because it avoids `PATH` issues entirely.

## Prerequisites

- [Rust stable](https://rustup.rs/) and Cargo
- [Codex CLI](https://github.com/openai/codex) installed and authenticated with `codex login`
- For `responses-only` mode: a valid `~/.codex/auth.json`

## Global Install

If you want a reusable shell command instead of `cargo run`, install it with Cargo:

```bash
cargo install --path .
```

Cargo installs the binary to `~/.cargo/bin`. If `claude-codex-proxy` still says `command not found`, add that directory to your `PATH`.

Temporary for the current shell:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
claude-codex-proxy serve
```

Permanent for `zsh`:

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
command -v claude-codex-proxy
```

## Quick Start

### Run from the repo

```bash
cargo run -- setup
cargo run -- serve
```

### Run after global install

```bash
claude-codex-proxy setup
claude-codex-proxy serve
```

### Configure a Claude-compatible client

Print shell exports:

```bash
claude-codex-proxy env --shell zsh
```

Typical output:

```bash
export ANTHROPIC_API_KEY='dummy'
export ANTHROPIC_BASE_URL='http://127.0.0.1:8080'
export ANTHROPIC_MODEL='gpt-5.3-codex-xhigh'
```

Apply them in your shell:

```bash
eval "$(claude-codex-proxy env --shell zsh)"
```

If you are running from the repo without global install:

```bash
eval "$(cargo run -- env --shell zsh)"
```

## Common Commands

```bash
claude-codex-proxy serve                 # start the proxy server
claude-codex-proxy setup                 # check codex binary, auth, handshake
claude-codex-proxy setup --write-config  # write Claude config when supported
claude-codex-proxy doctor                # diagnostics report
claude-codex-proxy doctor --json         # machine-readable diagnostics
claude-codex-proxy env                   # print client env config as JSON
claude-codex-proxy env --shell zsh       # print shell exports
```

Repo-local equivalents:

```bash
cargo run -- serve
cargo run -- setup
cargo run -- doctor --json
cargo run -- env --shell zsh
```

## Operation Modes

The proxy operates in three modes:

| Mode | Behavior |
| --- | --- |
| `auto-hybrid` | **Default.** App-server first; Responses API fallback for basic chat. |
| `strict-app-server` | No app-server = fail. All surfaces available. |
| `responses-only` | Legacy/debug. Stateless Responses API translation only. |

### App-server mode (primary)

Spawns `codex app-server` as a child process via stdio JSON-RPC. Manages Thread/Turn/Item lifecycle natively with approval/sandbox support at thread creation.

### Responses API mode (fallback)

Translates Anthropic/OpenAI requests to Codex Responses API via `chatgpt.com`. Used when app-server is unavailable in `auto-hybrid` mode.

## Task Completion & Thread Leasing (New)

The proxy now includes an in-process **JobExecutor** that bypasses the restrictive rate-limits of the stateless Responses API. It routes supported traffic directly to the underlying `app-server`, ensuring completely unbroken "run-to-completion" semantics for extensive tasks.

The proxy also introduces **Thread Leasing** (Experimental):

- By default, the app-server spawns a *fresh* sandboxed thread for every request to prevent context bleed.
- **Thread Leasing** allows explicit continuation flows (like updating a task or resuming a session) to "lease" the same execution thread, preserving conversation context and memory footprint.
- This is disabled by default to maintain safe stateless isolation, but can be enabled via configuration.

To activate, add these to your environment (see Configuration below for more):

```bash
export CLAUDE_CODEX_PROXY_ENABLE_THREAD_REUSE=true
```

## Configuration

| Flag | Env Var | Default | Description |
| --- | --- | --- | --- |
| `--port` | `PROXY_PORT` | `8080` | Listen port |
| `--auth-path` | `PROXY_AUTH_PATH` | `~/.codex/auth.json` | Auth file (responses-only mode) |
| `--mode` | — | `auto-hybrid` | Operation mode |
| `--app-server-experimental` | — | `false` | Opt in to experimental app-server API |
| `--delegation-policy` | — | `explicit-only` | Subagent spawn policy |
| `RUST_LOG` | — | `info` | Log level |
| `DISABLE_TOOL_FALLBACK` | — | `false` | Disable tool-stripping retry |
| `APP_SERVER_TURN_TIMEOUT_SECS` | — | `300` | Max wait for non-streaming turns |
| `CLAUDE_CODEX_PROXY_MAX_SESSIONS` | — | `256` | Max concurrent session entries |
| `CLAUDE_CODEX_PROXY_ENABLE_THREAD_REUSE` | — | `false` | Experimental: reuse threads for follow-up turns |

## Available Endpoints

### Core API

- `POST /v1/messages` — Anthropic Messages API
- `POST /v1/chat/completions` — OpenAI Chat Completions API
- `GET /v1/models` / `GET /models` — Model listing
- `GET /health` — Health check

### Bridge Diagnostics

- `GET /bridge/surfaces` — All known surfaces with mapping decisions
- `GET /bridge/surfaces/:id` — Surface detail
- `GET /bridge/compatibility` — Full compatibility matrix
- `GET /bridge/jobs` — Active job registry
- `GET /bridge/session/:id` — Session/thread state
- `GET /bridge/mode` — Operation mode + degradation status

## Surface Coverage

The proxy maps Claude Code surfaces across 6 tiers:

| Tier | Surfaces | Strategy |
| --- | --- | --- |
| 0 | Read, Write, Edit, MultiEdit, Glob, Grep, LS, Bash | `native` / `mediated_native` |
| 1 | Task*, Agent, SendMessage, AskUserQuestion, /sandbox, review family | `mediated_native` |
| 2 | Plan mode, Worktree, /resume, /rewind, /permissions | `mediated_native` |
| 3 | Cron*, Monitor, ToolSearch, WebFetch, WebSearch | `mediated_native` / `workflow_emulated` |
| 4 | /init, /memory, /mcp, /plugin, NotebookRead | `workflow_emulated` |
| 5 | Platform-specific, host UX | `unsupported_explicit` |

## Security Notes

- **Never** commit `.env` or `auth.json` to version control
- In app-server mode, approval policies and sandbox config are set at thread creation
- All side-effect surfaces (Write, Edit, Bash) go through the Approval Bridge
