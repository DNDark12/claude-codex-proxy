# claude-codex-proxy

A lightweight Rust proxy that bridges **Anthropic Messages** and **OpenAI Chat Completions** clients to the backend **ChatGPT Codex Responses API**.

## Key Features

- **Protocol Fidelity**: Fully supports both streaming and non-streaming responses.
- **Tool-Calling Support**: Maintains the complete tool-calling lifecycle seamlessly.
- **Malformed Tool Protection**: Automatically validates and filters out malformed `tool_use` parameters, preventing common "Invalid tool parameters" errors in Claude and OpenAI clients.
- **High Performance**: Built with Rust for speed, safety, and minimal resource usage.

## Prerequisites

- [Rust stable](https://rustup.rs/) and Cargo.
- A valid Codex authentication file (defaults to `~/.codex/auth.json`).

## Quick Start

1. **Clone the repository:**
   ```bash
   git clone https://github.com/DNDark12/claude-codex-proxy.git
   cd claude-codex-proxy
   ```

2. **Build the project:**
   ```bash
   cargo build --release
   ```

3. **Run the proxy:**
   ```bash
   ./target/release/claude-codex-proxy --port 8080 --auth-path ~/.codex/auth.json
   ```

4. **Verify it's running:**
   ```bash
   curl -s http://127.0.0.1:8080/health
   ```

## Configuration

The proxy automatically loads environment variables from a `.env` file if it exists.

1. **Create the environment file:**
   ```bash
   cp .env.example .env
   ```

2. **Adjust the variables** in `.env` as needed. You can then run the app directly using:
   ```bash
   cargo run --release
   ```

### Important Environment Variables

- `PROXY_PORT`: The local port for the proxy to listen on (default: `8080`).
- `PROXY_AUTH_PATH`: Absolute path to your `auth.json` (default: `~/.codex/auth.json`).
- `RUST_LOG`: Application log level (e.g., `info`, `debug`).
- `DISABLE_TOOL_FALLBACK`: Set to `true/1` to disable the tool-stripping retry logic.

## Usage with Claude Clients

You can expose the proxy as an Anthropic-compatible endpoint. For example, configure your Claude client (such as Claude Code) with:

```json
{
  "ANTHROPIC_API_KEY": "dummy",
  "ANTHROPIC_BASE_URL": "http://127.0.0.1:8080",
  "ANTHROPIC_MODEL": "gpt-5.4"
}
```

*Note: Actual model support depends on the backend capabilities at runtime. Use `GET /v1/models` to see the currently available models.*

## Available Endpoints

- `GET /health`
- `GET /models`
- `GET /v1/models`
- `POST /messages`
- `POST /v1/messages`
- `POST /chat/completions`
- `POST /v1/chat/completions`

## Security Notes

- **Never** commit your `.env` or `auth.json` files to version control.
- Ensure your `auth.json` file is kept secure locally; never share your authentication tokens.
