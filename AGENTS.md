# AGENTS.md — Claude Codex Proxy Development Guide

This guide helps AI agents quickly become productive in the **claude-codex-proxy** codebase.

---

## Project Overview

A high-performance Rust proxy that bridges **Anthropic Messages API** and **OpenAI Chat Completions API** clients to the **ChatGPT Codex Responses API** backend. The proxy translates protocol details (request/response shapes, streaming, tool-calling) while preserving semantic fidelity across three distinct LLM interfaces.

**Key value:**
- Developers use Claude or OpenAI clients against a single unified backend.
- Tool-calling lifecycle (function definition → invocation → result collection) works seamlessly across all three protocols.
- Malformed tool parameters are automatically filtered to prevent common "Invalid tool parameters" errors.

---

## Architecture — Four Layers

### 1. **Domain Models** (`src/domain/`)

Three distinct protocol shapes:
- `anthropic.rs` — Anthropic Messages API structures (Messages, Tools, Content Blocks, Tool Use)
- `openai.rs` — OpenAI Chat Completions shapes (Functions, ToolCalls, Content Parts)
- `codex.rs` — Codex Responses API format (Instructions, Input Items, Tool Definitions)
- `auth.rs` — Codex authentication token loading from `~/.codex/auth.json`

**Principle:** Each domain is self-contained serde de/serializable. No shared base types.

### 2. **Translation Layer** (`src/translation/`)

Four directional translators + utilities:
- `anthropic_to_codex.rs` — Convert Anthropic → Codex requests
- `codex_to_anthropic.rs` — Stream/collect Codex responses → Anthropic format (streaming & non-streaming paths)
- `openai_to_codex.rs` — Convert OpenAI → Codex requests
- `codex_to_openai.rs` — Stream/collect Codex → OpenAI format
- `tool_format.rs` — Tool schema normalization, aliasing, strategy mapping across three protocols
- `tool_runtime.rs` — Tool registry for resolving malformed parameters during tool execution

**Key insight:** Streaming and collection are separate code paths. Streaming returns `Box<dyn Stream>` for efficient memory; collection buffers into full responses. Both emit the client's expected format, not Codex format.

### 3. **Proxy Client** (`src/proxy/codex_client.rs`)

Makes authenticated requests to `https://chatgpt.com/backend-api/codex/*` endpoints.

**Features:**
- `CodexClient::from_auth_path()` — Loads auth token, initializes HTTP client with appropriate user-agent
- Models caching with 10-minute TTL to avoid repeated fetches
- Fallback model list when Codex endpoint is unreachable
- Streaming via `reqwest::Client` with tokio streams
- Upstream error classification: transport vs. HTTP status errors

### 4. **HTTP Routes & Request Dispatch** (`src/routes/mod.rs`)

Warp-based route handlers:
- `POST /v1/messages` and `/messages` — Anthropic entry points
- `POST /v1/chat/completions` and `/chat/completions` — OpenAI entry points
- `GET /models` and `/v1/models` — Model list endpoint
- `GET /health` — Health check

**Request flow:**
1. HTTP handler receives bytes + headers
2. Route deserializes into correct domain (Anthropic / OpenAI)
3. Calls `translate_*_to_codex()` with optional skill registry
4. Makes CodexClient request
5. Streams or collects response via `stream_*()` / `collect_*()`
6. Returns client's expected format

### 5. **Skills System** (`src/skills/`)

Optional skill distribution and runtime bridge:
- `manifest.rs` — YAML skill metadata (ID, description, references, merge mode)
- `loader.rs` — Load compiled skill registry JSON from disk
- `registry.rs` — SkillRegistry stores versioned skill definitions
- `resolver.rs` — Match Anthropic request markers to Codex skill instructions; merge tool definitions

**When active:** Skills modify the outgoing Codex request by injecting merged instructions and tool aliases.

---

## Critical Data Flows

### Streaming Response Path (Anthropic → Codex → Anthropic)

```
Client POST /v1/messages
  ↓
handle_anthropic_messages()
  ↓
translate_anthropic_to_codex(req, skill_registry)
  → Apply skill instructions if registry present
  ↓
CodexClient::stream_responses()
  → SSE stream from https://chatgpt.com/.../codex/responses
  ↓
stream_codex_to_anthropic(codex_stream)
  → Parse SSE events via event_extractor
  → Transform Codex Delta → Anthropic streaming format
  → Output as Server-Sent Events to client
  ↓
Client receives streamed Anthropic MessageStreamEvent messages
```

### Tool Execution Lifecycle

1. **Codex → Client:** Tool call instruction in response, client recognizes tool name
2. **Client → Proxy:** New request with tool result block (ToolResult with id + output)
3. **Proxy → Codex:** Convert to Codex FunctionCallOutput item in the input array
4. **Codex → Client:** Next turn of response with final answer
5. **Malformed parameters recovery:** If Codex tool execution fails with "Invalid tool parameters", the proxy strips all tools from a retry request (controlled by `DISABLE_TOOL_FALLBACK`)

---

## Build & Runtime

### Build

```bash
# Debug build (fast compile, slow runtime)
cargo build

# Release build (slow compile, optimized runtime)
cargo build --release
# Binary: ./target/release/claude-codex-proxy
```

### Run

**From `.env` file:**
```bash
cp .env.example .env
# Edit PROXY_PORT, PROXY_AUTH_PATH, RUST_LOG
cargo run --release
```

**Via CLI args:**
```bash
./target/release/claude-codex-proxy \
  --port 8080 \
  --auth-path ~/.codex/auth.json \
  --skills-registry-path ./skills_registry.json
```

**Environment resolution order:**
- CLI arg → env var → hardcoded default
- Auth path default: `~/.codex/auth.json`
- Port default: `8080`
- Skills registry: optional (skipped if not provided)

### Health Check

```bash
curl -s http://127.0.0.1:8080/health
# Response: {"status": "ok"}
```

---

## Node.js Tooling (Skill Compilation)

Two complementary packages:

### `packages/skill-compiler/` — Compile Canonical Skills

Transforms a source skill directory into Claude and Codex artifacts.

**Usage:**
```bash
cd packages/skill-compiler
node --test tests/*.test.mjs
```

**Inputs:**
- `skill.yaml` — Skill metadata (id, description, references, artifacts config)
- `claude.md` — Claude-specific prompt
- `references/` — Supporting files bundled with skill

**Outputs:**
- Codex markdown skill file
- Claude plugin JSON
- Claude command markdown
- Reference bundle JSON
- Registry entry JSON

### `packages/skills-cli/` — Install Skills Locally

Project-local installer for skill distribution.

**Usage:**
```bash
cd packages/skills-cli
node --test tests/*.test.mjs
```

---

## Key Conventions & Patterns

### Error Handling

- **Upstream errors:** Distinguish `UpstreamError::Transport` (network/connection) vs. `UpstreamError::Upstream { status, body }` (HTTP error from Codex)
- **Anyhow context:** Use `.context()` to add layers of meaning to errors before propagating
- **Logging:** `log::info!`, `log::warn!`, `log::error!` respect `RUST_LOG` env var

### Tool Parameter Validation

**Location:** `src/translation/tool_runtime.rs`

When Codex returns a tool error about malformed parameters:
1. Extract problematic tool names
2. Build a filtered tool list (only valid tools)
3. Retry Codex request without the bad tools
4. Return filtered response to client

**Disable via:** `DISABLE_TOOL_FALLBACK=true`

### Streaming Architecture

- **Codex → Proxy:** SSE parser splits raw bytes into event chunks
- **Event extraction:** Identifies delta / error / stop events
- **Format transformation:** Codex delta structure → client's expected format
- **Async buffering:** `tokio-stream` + `futures-util` for efficient backpressure

### Skill Registry Merging

**Location:** `src/skills/resolver.rs`

When a skill is active:
- Extract references (doc URLs, tools) from skill manifest
- Merge tool definitions: skill tools + request tools (skill takes precedence)
- Inject merged instructions into Codex request
- Preserve tool aliases for name translation

---

## Testing

### Rust Tests

No dedicated test suite yet. QA via:
- Type checking: `cargo check`
- Linting: `cargo clippy`
- Manual integration tests (run proxy locally, curl endpoints)

### Node.js Package Tests

```bash
# Skill compiler tests
cd packages/skill-compiler && node --test tests/*.test.mjs

# Skills CLI tests
cd packages/skills-cli && node --test tests/*.test.mjs
```

---

## Common Tasks

### Add a New Endpoint

1. Define request/response types in `src/domain/`
2. Create translation function in `src/translation/`
3. Add handler in `src/routes/mod.rs`
4. Register route filter in `build_routes()`
5. Test via curl or client library

### Fix Tool Parameter Validation

1. Identify problematic tool schema in `src/translation/tool_format.rs`
2. Update `normalize_schema()` or aliasing logic
3. Test with both Anthropic and OpenAI client formats

### Add Skill Support

1. Update skill manifest in `skills/*/skill.yaml`
2. Regenerate artifacts via skill-compiler
3. Place registry JSON at `--skills-registry-path`
4. Restart proxy; it loads on startup

### Debug Streaming Responses

1. Set `RUST_LOG=debug` for detailed event logs
2. Check `src/proxy/event_extractor.rs` for SSE parsing issues
3. Compare Codex raw response with translated output in `src/translation/codex_to_*.rs`

---

## Project Structure Summary

```
src/
  main.rs               — Argument parsing, initialization, server startup
  domain/               — Protocol-specific data types (Anthropic, OpenAI, Codex, Auth)
  proxy/                — HTTP client to Codex, SSE streaming, event parsing
  routes/               — Warp HTTP route handlers, request dispatch
  skills/               — Skill registry loading and resolution
  translation/          — Protocol bridging: Anthropic ↔ Codex ↔ OpenAI

packages/
  skill-compiler/       — Node.js: transforms canonical skills → Claude + Codex artifacts
  skills-cli/           — Node.js: project-local skill installer

docs/
  claude-codex-skill-bridge-plan.md  — Design for skill distribution system
  claude-codex-capability-bridge-plan.md — Design for capability mapping
  [other design docs]

skills/
  code-review/          — Example skill (source + compiled artifacts)
```

---

## Performance & Safety Notes

- **Concurrency:** Tokio async runtime handles many simultaneous streams. Models cache uses `Arc<RwLock>` for thread-safe, lock-free reads.
- **Memory:** Streaming avoids buffering entire responses; only collection buffers (used when stream=false).
- **Tool safety:** Malformed parameters are silently filtered; proxy never crashes on bad tool JSON from Codex.
- **Auth:** Codex auth token loaded once at startup; no re-fetching. Keep `.codex/auth.json` secure locally.

---

## When Stuck

1. **Protocol mismatch:** Check if you're in the right translator direction (anthropic_to_codex vs. openai_to_codex)
2. **Streaming not working:** Verify event_extractor is parsing SSE correctly; check Codex response format
3. **Tools not invoked:** Check tool_format.rs aliasing; Codex may not recognize aliased names
4. **Auth failures:** Verify `.codex/auth.json` format and expiry; check logs for HTTP 401
5. **Tests fail:** Run `cargo clippy` and `cargo check` first; type errors often reveal the issue


