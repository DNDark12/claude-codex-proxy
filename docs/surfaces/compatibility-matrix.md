# Compatibility Matrix

This matrix is the static policy baked into `src/surfaces/matrix.rs`.

| Surface Class | `strict-app-server` / `auto-hybrid` | `responses-only` | Fallback |
|---|---|---|---|
| Tier 0 core tools | `native` or `mediated_native` via app-server | degraded to Responses API translation | `soft_warning_and_continue` or `hard_error` |
| Tier 1 runtime workflows | app-server-first or workflow-emulated | unsupported | `hard_error` or `downgrade_to_workflow` |
| Tier 2 workspace/planning | app-server-first | unsupported | `hard_error` or `downgrade_to_workflow` |
| Tier 3 scheduling/web | mediated-native or emulated | only partial web/read surfaces degrade | `soft_warning_and_continue` |
| Tier 4 guidance/MCP | mediated-native or workflow-emulated | mostly unsupported | `downgrade_to_workflow` |
| Host-admin UX / out-of-scope | dropped with telemetry | dropped with telemetry | `drop_with_observability` |

## Explicit Policies

- `host_admin_ux` and `out_of_scope` surfaces always map to `drop_with_observability`.
- `platform_specific` surfaces return `unsupported_explicit` unless their availability gate is satisfied.
- `responses-only` can only degrade stateless or tool-light surfaces. Thread-native workflows are rejected explicitly.
