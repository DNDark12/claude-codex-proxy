# BUG REPORT

## Title
Missing public model profiles for Codex `high` / `xhigh` reasoning caused incomplete client config and dropped effort on app-server turns.

## Symptoms
- Public model discovery did not expose reasoning-aware model profiles.
- `env` / setup-driven client configuration had no visible `high` / `xhigh` equivalents for discovered models.
- Non-stream app-server requests ignored reasoning effort even when the caller encoded it via model alias or explicit request fields.

## Evidence
- `cargo run -- doctor --json` originally showed only base app-server models such as `gpt-5.2-codex`, `gpt-5.1-codex-max`, `gpt-5.2`, `gpt-5.1-codex-mini`.
- Red-phase regression tests failed because translation kept alias models like `gpt-5.2-codex-high` unchanged instead of splitting them into `backend model + effort`.

## Root Cause
The proxy had no normalization layer for reasoning-profile model aliases.

As a result:
- public model listing surfaced only raw backend models,
- translation forwarded alias strings as literal backend model ids,
- app-server `turn/start` never sent the `effort` field even though the stable schema supports it.

## Fix
- Added `src/model_profiles.rs` to normalize aliases such as `-high`, `-xhigh`, and `-extra-high`.
- Expanded public model catalogs to include reasoning profiles for discovered base models.
- Applied normalized `backend model + effort` in both Anthropic/OpenAI translation paths.
- Forwarded `effort` and `summary=auto` through app-server `turn/start`.
- Updated CLI model discovery used by `env`.

## Verification
- `cargo test reasoning_model_alias`
- `cargo test over_model_alias`
- `cargo test`
- `cargo run -- doctor --json`

## Residual Risk
- `doctor` still reports the app-server-centric catalog under `appServer.models`; the full hybrid union is exposed through runtime model discovery used by the proxy and CLI config flow, but not broken out as a separate field in the doctor payload.
