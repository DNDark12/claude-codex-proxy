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

`claude-codex-proxy` does **not** require the Codex desktop app to stay open. It spawns `codex app-server` from the CLI as a child process. Keeping the desktop app open is fine, but not required.

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

## Full Function Setup

For the full feature set, use:

1. `auto-hybrid` mode
2. a multi-account pool in `accounts.toml`
3. JSONL persistence for jobs/sessions
4. thread reuse enabled for explicit continuation flows

Example local setup:

```bash
cp .env.example .env
cp accounts.toml.example accounts.toml
mkdir -p data
```

Recommended env values:

```bash
PROXY_PORT=8080
PROXY_AUTH_PATH=~/.codex/auth.json
ACCOUNTS_CONFIG_PATH=./accounts.toml
ENABLE_TOKEN_PASTE=true
APP_SERVER_TURN_TIMEOUT_SECS=300
CLAUDE_CODEX_PROXY_JSONRPC_TIMEOUT_SECS=300
CLAUDE_CODEX_PROXY_MAX_SESSIONS=256
CLAUDE_CODEX_PROXY_JOBS_JSONL=./data/jobs.jsonl
CLAUDE_CODEX_PROXY_SESSIONS_JSONL=./data/sessions.jsonl
CLAUDE_CODEX_PROXY_ENABLE_THREAD_REUSE=true
CLAUDE_CODEX_PROXY_THREAD_IDLE_SECS=1800
CLAUDE_CODEX_PROXY_THREAD_MAX_TURNS=8
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
| `CLAUDE_CODEX_PROXY_JSONRPC_TIMEOUT_SECS` | — | `APP_SERVER_TURN_TIMEOUT_SECS` (fallback) | Max wait for each app-server JSON-RPC request |
| `CLAUDE_CODEX_PROXY_MAX_SESSIONS` | — | `256` | Max concurrent session entries |
| `CLAUDE_CODEX_PROXY_JOBS_JSONL` | — | unset | Persist job metadata to a JSONL file |
| `CLAUDE_CODEX_PROXY_SESSIONS_JSONL` | — | unset | Persist session metadata to a JSONL file |
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

## Persistence

To persist bridge state across proxy restarts, point these env vars at writable JSONL files:

```bash
export CLAUDE_CODEX_PROXY_JOBS_JSONL="$HOME/.codex/proxy/jobs.jsonl"
export CLAUDE_CODEX_PROXY_SESSIONS_JSONL="$HOME/.codex/proxy/sessions.jsonl"
```

The proxy snapshots `JobRegistry` and `StateStore` on every write. Leave them unset if you want purely in-memory runtime state.

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

---

## Multi-Account Pool

Proxy hỗ trợ nhiều tài khoản Codex xoay vòng tự động (round-robin) với health tracking.

### Setup

```bash
# 1. Copy config mẫu
cp accounts.toml.example accounts.toml

# 2. Điền thông tin tài khoản
# Mỗi auth.json tương ứng 1 tài khoản ChatGPT Plus/Codex
```

`accounts.toml`:
```toml
[pool]
error_threshold = 3    # degraded sau 3 lỗi liên tiếp
cooldown_secs = 120    # tự recover sau 2 phút

[[account]]
id = "account_1"
label = "Account Chính"
auth_path = "~/.codex/auth_1.json"
enabled = true

[[account]]
id = "account_2"
label = "Account Phụ"
auth_path = "~/.codex/auth_2.json"
enabled = true
```

**Load priority:**

1. `ACCOUNTS_CONFIG_PATH` env var
2. `accounts.toml` trong thư mục hiện tại
3. `PROXY_AUTH_PATH` (single account, backward compat)

### Account rotation logic

- Round-robin qua tất cả `enabled` accounts
- Nếu 1 account lỗi liên tiếp ≥ `error_threshold` → **degraded** (cooldown)
- Nếu app-server/background job trả về quota / `429` / `quota exceeded` → account đó bị hold theo `Retry-After` hoặc thời gian suy ra từ message lỗi, rồi proxy tự roll sang account còn available
- Nếu nhiều `auth.json` thực ra là cùng một account (`account_id`/email giống nhau), proxy sẽ ưu tiên bản auth mới hơn và bỏ qua alias stale khi chọn account
- Sau `cooldown_secs` → tự động thử lại
- Nếu tất cả accounts degraded → log warning, request fail

### Scan và persistence

- Trong UI `/accounts`, nút `Scan` sẽ scan các auth file đang có và **tự đồng bộ vào pool**
- `Scan` hiện đọc các auth file từ `PROXY_AUTH_PATH`, `$CODEX_HOME/auth.json`, `$CODEX_HOME/proxy-accounts/*/auth.json`, và `$CODEX_HOME/multi-auth/projects/*/auth.json`
- Khi `CLAUDE_CODEX_PROXY_ACCOUNT_AUTO_SYNC=true`, proxy cũng tự rescan định kỳ trước lúc dispatch request; bạn không cần bấm `Scan` chỉ để pickup một `auth.json` mới hơn
- Account được thêm/toggle/remove qua UI sẽ được persist về `accounts.toml` hoặc `ACCOUNTS_CONFIG_PATH`
- Khi restart service, danh sách pool sẽ được load lại từ file config đã persist
- Nếu cùng một `auth_path` được login lại bằng account khác rồi `Scan`, proxy sẽ refresh metadata, clear stale quota/auth penalty của entry đó, và request kế tiếp sẽ khởi động lại app-server runtime theo auth file mới
- Nếu một account có nhiều alias path và một alias được refresh mới hơn, proxy sẽ mirror auth mới sang alias cũ của cùng account khi auto-sync/manual sync chạy; điều này giúp session đang bám path cũ tiếp tục có credential mới

### Auth refresh reality

- Proxy **không tự mint refresh token mới** từ một refresh token đã bị upstream vô hiệu hóa
- Proxy chỉ có thể tự pickup hoặc mirror một `auth.json` mới hơn đã tồn tại trên máy
- Nếu mọi bản `auth.json` của account đều đã rơi vào `refresh token was already used` / `invalid_grant`, bạn vẫn cần chạy `codex login` một lần cho account đó để tạo credential mới
- Codex desktop app **không bắt buộc**; `codex` CLI + `codex login` là đủ cho full functionality của proxy

### Admin API

```bash
# List accounts + stats
GET /api/accounts

# Add account tại runtime (không cần restart)
POST /api/accounts
{"id": "acc3", "auth_path": "~/.codex/auth_3.json", "label": "Account 3"}

# Enable/disable
PATCH /api/accounts/account_1/toggle

# Clear error state
POST /api/accounts/account_1/reset

# Remove
DELETE /api/accounts/account_1

# Auto-sync discovered accounts into the pool
POST /api/accounts/sync
```

---

## Web UI

Sau khi start server, truy cập:

| URL | Mô tả |
|-----|-------|
| `http://localhost:8080/` | Dashboard — overview |
| `http://localhost:8080/accounts` | Quản lý accounts, add/remove/toggle |
| `http://localhost:8080/sessions` | Jobs đang chạy, surfaces, compatibility |

---

## Docker

### Quick start

```bash
# 1. Tạo config dir
mkdir config
cp accounts.toml.example config/accounts.toml
cp ~/.codex/auth.json config/auth.json

# 2. Chạy
docker compose up -d

# 3. Xem log
docker compose logs -f proxy

# 4. Mở UI
open http://localhost:8080/ui/
```

### Build image thủ công

```bash
docker build -t claude-codex-proxy .
docker run -d \
  -p 8080:8080 \
  -v $(pwd)/config:/config:ro \
  -v proxy_data:/data \
  claude-codex-proxy
```

### Dev mode (hot reload)

```bash
docker compose --profile dev up proxy-dev
# Proxy chạy ở localhost:8081, tự rebuild khi sửa code
```

---

## Persistent State

Session và job state hiện được persist bằng JSONL nếu bạn set:

```bash
CLAUDE_CODEX_PROXY_JOBS_JSONL=./data/jobs.jsonl
CLAUDE_CODEX_PROXY_SESSIONS_JSONL=./data/sessions.jsonl
```

Để trống hai biến này nếu bạn chỉ muốn state in-memory.

---

## Configuration Reference (Updated)

| Env Var | Default | Mô tả |
|---------|---------|-------|
| `PROXY_PORT` | `8080` | Listen port |
| `ACCOUNTS_CONFIG_PATH` | `./accounts.toml` khi set | Path tới accounts.toml |
| `PROXY_AUTH_PATH` | `~/.codex/auth.json` | Single account fallback |
| `ENABLE_TOKEN_PASTE` | `false` | Cho phép add account bằng raw auth/token qua UI/API |
| `RUST_LOG` | `info` | Log level |
| `DISABLE_TOOL_FALLBACK` | `false` | Tắt tool-stripping retry |
| `APP_SERVER_TURN_TIMEOUT_SECS` | `300` | Timeout non-streaming turn |
| `CLAUDE_CODEX_PROXY_JSONRPC_TIMEOUT_SECS` | `APP_SERVER_TURN_TIMEOUT_SECS` | Timeout cho từng JSON-RPC request tới app-server |
| `CLAUDE_CODEX_PROXY_MAX_SESSIONS` | `256` | Max session entries |
| `CLAUDE_CODEX_PROXY_JOBS_JSONL` | unset | JSONL path cho job persistence |
| `CLAUDE_CODEX_PROXY_SESSIONS_JSONL` | unset | JSONL path cho session persistence |
| `CLAUDE_CODEX_PROXY_ACCOUNT_AUTO_SYNC` | `true` | Tự rescan discovered auth files trước dispatch |
| `CLAUDE_CODEX_PROXY_ACCOUNT_AUTO_SYNC_INTERVAL_SECS` | `30` | Khoảng cách tối thiểu giữa hai lần auto-sync |
| `CLAUDE_CODEX_PROXY_ENABLE_THREAD_REUSE` | `false` | Bật explicit continuation/thread leasing |
| `CLAUDE_CODEX_PROXY_THREAD_IDLE_SECS` | `1800` | Idle timeout cho leased thread |
| `CLAUDE_CODEX_PROXY_THREAD_MAX_TURNS` | `8` | Max follow-up turns trên 1 leased thread |

---

## Security Notes (Updated)

- **Không commit** `.env`, `accounts.toml`, `auth*.json` lên git
- UI và Admin API (`/`, `/accounts`, `/sessions`, `/api/accounts/*`) chỉ expose trên localhost theo mặc định
- Nếu expose ra ngoài: dùng reverse proxy (nginx/caddy) với authentication
- Token trong `auth.json` là credential của tài khoản ChatGPT Plus — bảo vệ như password
