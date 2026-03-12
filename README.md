# Claude Codex Proxy

Proxy Rust để bridge client kiểu **Anthropic Messages** hoặc **OpenAI Chat Completions** sang backend **ChatGPT Codex Responses API**.

Mục tiêu chính của project hiện tại:
- Giữ **protocol fidelity** cho stream/non-stream.
- Giữ đầy đủ vòng **tool-calling**.
- Tránh phát `tool_use` malformed (nguồn gây `Invalid tool parameters`).

## Kiến trúc

Code được tách theo 4 lớp:

- `src/routes/`
  - Nhận request theo protocol (`/v1/messages`, `/v1/chat/completions`), validate JSON, map lỗi đúng schema theo protocol.
- `src/translation/`
  - Inbound mapping: Anthropic/OpenAI -> Codex request.
  - Outbound mapping: Codex SSE -> Anthropic/OpenAI stream + non-stream payload.
  - `tool_runtime.rs`: Tool registry, assembler, validation JSON/schema, protocol debug.
- `src/proxy/`
  - HTTP client gọi upstream Codex backend.
  - SSE parser theo block (`\n\n`) + event extractor typed.
- `src/domain/`
  - Kiểu dữ liệu cho Anthropic/OpenAI/Codex/Auth.

## Luồng xử lý

1. Client gọi `POST /v1/messages` hoặc `POST /v1/chat/completions`.
2. Route parse request, tạo `ToolRegistry` từ tools gốc của client.
3. Translator map request sang Codex Responses API.
4. Proxy gọi `https://chatgpt.com/backend-api/codex/responses` (stream nội bộ luôn bật).
5. Event extractor tách text/tool delta/done/usage/error.
6. Outbound translator dựng lại stream/payload theo đúng protocol client.

## Cơ chế chống `Invalid tool parameters`

Project dùng `ToolCallAssembler` để lắp call theo `call_id` và chỉ emit tool khi hợp lệ:

- Ưu tiên parse từ `delta_buffer`.
- Nếu delta lỗi JSON thì fallback sang `done.arguments`.
- Validate theo schema tool gốc từ request (`ToolRegistry`).
- Nếu fail JSON/schema hoặc không map được schema:
  - **Không emit `tool_use` malformed**.
  - Emit text diagnostic ngắn có `trace` để debug.

Kết quả:
- Claude/OpenAI client không bị văng lỗi ngay do tool call sai định dạng.
- Debug được root cause ở log nếu bật protocol debug.

## Streaming contract

### Anthropic
- Emit theo chuỗi:
  - `message_start`
  - `content_block_start/delta/stop`
  - `message_delta`
  - `message_stop`
- `stop_reason`:
  - `tool_use` nếu có >=1 tool_use hợp lệ.
  - `end_turn` nếu không có tool_use hợp lệ.
- Có recovery khi upstream kết thúc thiếu marker (`force finalize`).

### OpenAI
- Chunk đầu có `role=assistant`.
- Tool chunks phát qua `delta.tool_calls`.
- Chunk cuối có `finish_reason`.
- Luôn có `[DONE]`.

## Yêu cầu

- Rust stable + Cargo.
- File auth Codex hợp lệ (mặc định `~/.codex/auth.json`).

## Quick Start

```bash
git clone <repo-url>
cd codex-openai-proxy
cargo build --release
./target/release/claude-codex-proxy --port 8080 --auth-path ~/.codex/auth.json
```

Health check:

```bash
curl -s http://127.0.0.1:8080/health
```

## Cấu hình bằng `.env`

Binary **tự động load `.env`** (nếu file tồn tại) khi startup.

Thứ tự ưu tiên config:
- CLI args (`--port`, `--auth-path`)
- ENV (`PROXY_PORT`, `PROXY_AUTH_PATH`)
- Default (`8080`, `~/.codex/auth.json`)

1. Tạo file env:

```bash
cp .env.example .env
```

2. Chỉnh các biến cần thiết trong `.env`.

3. Chạy trực tiếp binary/app:

```bash
cargo run --release
```

Hoặc vẫn có thể chạy script helper nếu muốn:

```bash
./scripts/run-with-env.sh
```

### Biến môi trường hỗ trợ

- `PROXY_PORT`
  - Port chạy local proxy (dùng trong script).
- `PROXY_AUTH_PATH`
  - Path tới `auth.json` (dùng trong script).
- `RUST_LOG`
  - Mức log app (`info`, `debug`, ...).
- `LOG_PROTOCOL_DEBUG`
  - `true/1`: bật log protocol debug (đã scrub).
- `DISABLE_TOOL_FALLBACK`
  - `true/1`: tắt retry strip-tools.
  - `false/0`: cho retry 1 lần chỉ khi upstream báo tools unsupported và tool_choice không bắt buộc.

## Endpoint công khai

- `GET /health`
- `GET /models`
- `GET /v1/models`
- `POST /messages`
- `POST /v1/messages`
- `POST /chat/completions`
- `POST /v1/chat/completions`

## Cấu hình Claude CLI/Claude Code

Ví dụ cấu hình trỏ Anthropic base URL về proxy:

```json
{
  "ANTHROPIC_API_KEY": "dummy",
  "ANTHROPIC_BASE_URL": "http://127.0.0.1:8080",
  "ANTHROPIC_MODEL": "gpt-5.4"
}
```

Lưu ý: model thực tế phụ thuộc capability backend tại thời điểm chạy. Dùng `GET /v1/models` để xem danh sách hiện tại.

## Debug nhanh

Bật protocol debug:

```bash
LOG_PROTOCOL_DEBUG=true RUST_LOG=info cargo run --release
```

Khi đó log sẽ có các trường:
- `trace_id`, `request_id`, `response_id`
- `call_id`, `tool_name`, `source(delta/done)`
- `json_valid`, `schema_valid`, `emit`, `reason`

Không dump raw token/prompt/tool args để tránh lộ dữ liệu nhạy cảm.

## Dev

```bash
cargo fmt
cargo test
cargo clippy
```

## Ghi chú bảo mật

- Không commit `.env` và `auth.json`.
- Chỉ dùng `auth.json` từ máy bạn, không chia sẻ token.
