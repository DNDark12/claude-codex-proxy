# Claude Codex Proxy

A proxy server that bridges **Claude Code** (and other OpenAI/Anthropic-compatible extensions) to the **ChatGPT Plus / Codex** backend. This allows you to use your existing ChatGPT Plus tokens directly within Claude Code instead of requiring separate Anthropic API keys.

## Overview

This proxy acts as a universal bridge:
- **Input**: Anthropic SDK Messages API (`/v1/messages`) AND standard OpenAI Chat Completions API (`/v1/chat/completions`).
- **Output**: ChatGPT Responses API format (what the ChatGPT backend actually uses).

## Features

- ✅ **Claude Code Support**: Fully compatible with Anthropic's Messages format, converting tool parameters and system instructions perfectly.
- ✅ **OpenAI API Compatibility**: Accepts standard OpenAI Chat Completions requests alongside Anthropic requests.
- ✅ **ChatGPT Plus Integration**: Uses your existing ChatGPT Plus access tokens.
- ✅ **Cloudflare Bypass**: Handles ChatGPT's Cloudflare protection with browser-like headers.
- ✅ **Streaming Responses**: Full real-time SSE streaming support for both OpenAI and Anthropic response styles.
- ✅ **Bulletproof Content Duplication Fix**: Rewritten SSE parser ensures clean responses without duplicating chunks.

## Quick Start

### 1. Build and Run

```bash
git clone https://github.com/Securiteru/claude-codex-proxy.git
cd claude-codex-proxy

# Build for release
cargo build --release

# Run on port 8888 (make sure to point to your valid auth.json)
./target/release/claude-codex-proxy --port 8888 --auth-path ~/.codex/auth.json
```

### 2. Configure Claude Code

Update your Claude Code `~/.claude/settings.json` to point the Anthropic base URL to the proxy.

```json
{
  "ANTHROPIC_API_KEY": "any-random-string",
  "ANTHROPIC_BASE_URL": "http://127.0.0.1:8888",
  "ANTHROPIC_DEFAULT_SONNET_MODEL": "gpt-5.4",
  "ANTHROPIC_DEFAULT_OPUS_MODEL": "gpt-5.4",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL": "gpt-5.4",
  "ANTHROPIC_MODEL": "gpt-5.4",
  "theme": "dark"
}
```

The model can be set to whichever model the Codex backend currently supports (e.g., `gpt-5.4` or `gpt-4o`).

## How It Works

### Request Flow

1. **Claude Code** → Anthropic Messages format `/v1/messages` → **Proxy**
2. **Proxy** → Converts to ChatGPT Responses API → **ChatGPT Backend**
3. **ChatGPT Backend** → Real-time SSE Responses API stream → **Proxy**
4. **Proxy** → Converts back to Anthropic SSE style → **Claude Code**

### Format Conversion Rules

When Claude Code requests tool usage, the proxy handles the structural changes:
* Anthropic's `input_schema` is remapped to OpenAI's `parameters`.
* Tools are stripped from the final Codex request to prevent `400 Bad Request` backend errors (as the current unofficial endpoint does not cleanly support structured tool execution).
* System messages are correctly routed to the `instructions` field.

## Configuration

### Command Line Options

```bash
claude-codex-proxy [OPTIONS]

Options:
  -p, --port <PORT>          Port to listen on [default: 8080]
      --auth-path <PATH>     Path to Codex auth.json [default: ~/.codex/auth.json]
  -h, --help                 Print help
```

### Authentication

The proxy automatically reads authentication from your Codex `auth.json` file usually found in `~/.codex/auth.json`:

```json
{
  "access_token": "eyJ...",
  "account_id": "db1fc050-5df3-42c1-be65-9463d9d23f0b"
}
```

**Priority**: Uses `access_token` + `account_id` for ChatGPT Plus accounts to hit the backend directly.

## API Endpoints

### Health Check
- **GET** `/health`
- Returns service status (useful for uptime monitoring)

### Anthropic Messages
- **POST** `/v1/messages` and `/messages`
- Anthropic-compatible chat endpoint utilized by Claude Code.

### OpenAI Chat Completions
- **POST** `/v1/chat/completions` and `/chat/completions`
- OpenAI-compatible chat completions endpoint.

## Development

### Building & Testing

```bash
cargo build
cargo test
cargo clippy
cargo fmt
```

### Troubleshooting

If you encounter `500 Backend Error` or duplicated responses, run the proxy directly instead of detaching it, so you can see `eprintln!` standard error logs:

```bash
pkill -9 claude-codex-proxy
cargo run --release -- --port 8888 --auth-path ~/.codex/auth.json
```

## License

This project follows standard open-source licensing.