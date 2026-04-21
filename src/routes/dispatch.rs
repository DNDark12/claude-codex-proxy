use crate::domain::anthropic::AnthropicMessagesRequest;
use crate::domain::openai::ChatCompletionsRequest;
use crate::surfaces::{ClassifiedSurface, CompatibilityMatrix, OperationMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchBackend {
    AppServer,
    ResponsesFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    AttachedStream,
    AttachedCollect,
    DetachedBackground,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchPlan {
    pub backend: DispatchBackend,
    pub execution_mode: ExecutionMode,
    pub has_task_runtime_surface: bool,
}

pub struct DispatchPlanner;

impl DispatchPlanner {
    fn plan_core(
        stream: bool,
        has_task_runtime_surface: bool,
        mode: OperationMode,
        app_server_available: bool,
    ) -> DispatchPlan {
        if !app_server_available || matches!(mode, OperationMode::ResponsesOnly) {
            return DispatchPlan {
                backend: DispatchBackend::ResponsesFallback,
                execution_mode: if stream {
                    ExecutionMode::AttachedStream
                } else {
                    ExecutionMode::AttachedCollect
                },
                has_task_runtime_surface: false,
            };
        }

        let execution_mode = if has_task_runtime_surface {
            ExecutionMode::DetachedBackground
        } else if stream {
            ExecutionMode::AttachedStream
        } else {
            ExecutionMode::AttachedCollect
        };

        DispatchPlan {
            backend: DispatchBackend::AppServer,
            execution_mode,
            has_task_runtime_surface,
        }
    }

    pub fn plan_anthropic(
        request: &AnthropicMessagesRequest,
        surfaces: &[ClassifiedSurface],
        mode: OperationMode,
        app_server_available: bool,
        _matrix: &CompatibilityMatrix,
    ) -> DispatchPlan {
        Self::plan_core(
            request.stream.unwrap_or(false),
            has_task_runtime_surface(surfaces),
            mode,
            app_server_available,
        )
    }

    pub fn plan_openai(
        request: &ChatCompletionsRequest,
        surfaces: &[ClassifiedSurface],
        mode: OperationMode,
        app_server_available: bool,
        _matrix: &CompatibilityMatrix,
    ) -> DispatchPlan {
        Self::plan_core(
            request.stream.unwrap_or(false),
            has_task_runtime_surface(surfaces),
            mode,
            app_server_available,
        )
    }
}

fn has_task_runtime_surface(surfaces: &[ClassifiedSurface]) -> bool {
    surfaces.iter().any(|surface| {
        matches!(
            surface.surface_id.as_deref(),
            Some("tool.taskcreate")
                | Some("tool.taskget")
                | Some("tool.tasklist")
                | Some("tool.taskupdate")
                | Some("tool.taskstop")
                | Some("tool.agent")
                | Some("tool.sendmessage")
                | Some("workflow.code_review")
                | Some("workflow.security_review")
                | Some("workflow.rescue_fix")
                | Some("command.tasks")
                | Some("command.security_review")
                | Some("command.resume")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surfaces::{CompatibilityMatrix, OperationMode, SurfaceClassifier, SurfaceRegistry};

    #[test]
    fn anthropic_streaming_tools_prefers_app_server_stream_when_available() {
        let registry = SurfaceRegistry::new();
        let classifier = SurfaceClassifier::new(registry.clone());
        let matrix = CompatibilityMatrix::new(&registry);
        let request: crate::domain::anthropic::AnthropicMessagesRequest =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-5.4",
                "stream": true,
                "tools": [{ "name": "Read", "input_schema": { "type": "object" } }],
                "messages": [{ "role": "user", "content": "inspect the repo" }]
            }))
            .unwrap();

        let plan = DispatchPlanner::plan_anthropic(
            &request,
            &classifier.classify_anthropic_request(&request),
            OperationMode::AutoHybrid,
            true,
            &matrix,
        );

        assert_eq!(plan.backend, DispatchBackend::AppServer);
        assert_eq!(plan.execution_mode, ExecutionMode::AttachedStream);
    }

    #[test]
    fn anthropic_task_surface_prefers_detached_background() {
        let registry = SurfaceRegistry::new();
        let classifier = SurfaceClassifier::new(registry.clone());
        let matrix = CompatibilityMatrix::new(&registry);
        let request: crate::domain::anthropic::AnthropicMessagesRequest =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-5.4",
                "stream": true,
                "tools": [{ "name": "TaskCreate", "input_schema": { "type": "object" } }],
                "messages": [{ "role": "user", "content": "create a background task" }]
            }))
            .unwrap();

        let plan = DispatchPlanner::plan_anthropic(
            &request,
            &classifier.classify_anthropic_request(&request),
            OperationMode::AutoHybrid,
            true,
            &matrix,
        );

        assert_eq!(plan.backend, DispatchBackend::AppServer);
        assert_eq!(plan.execution_mode, ExecutionMode::DetachedBackground);
    }

    #[test]
    fn falls_back_to_responses_when_app_server_is_unavailable() {
        let registry = SurfaceRegistry::new();
        let classifier = SurfaceClassifier::new(registry.clone());
        let matrix = CompatibilityMatrix::new(&registry);
        let request: crate::domain::openai::ChatCompletionsRequest =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-5.4",
                "stream": true,
                "messages": [{ "role": "user", "content": "hello" }]
            }))
            .unwrap();

        let plan = DispatchPlanner::plan_openai(
            &request,
            &classifier.classify_openai_request(&request),
            OperationMode::AutoHybrid,
            false,
            &matrix,
        );

        assert_eq!(plan.backend, DispatchBackend::ResponsesFallback);
    }

    #[test]
    fn task_runtime_surfaces_choose_app_server_even_when_responses_are_rate_limited() {
        let registry = SurfaceRegistry::new();
        let classifier = SurfaceClassifier::new(registry.clone());
        let matrix = CompatibilityMatrix::new(&registry);
        let request: crate::domain::anthropic::AnthropicMessagesRequest =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-5.4",
                "stream": true,
                "tools": [{ "name": "TaskCreate", "input_schema": { "type": "object" } }],
                "messages": [{ "role": "user", "content": "create a background task" }]
            }))
            .unwrap();

        let plan = DispatchPlanner::plan_anthropic(
            &request,
            &classifier.classify_anthropic_request(&request),
            OperationMode::AutoHybrid,
            true,
            &matrix,
        );

        assert_eq!(plan.backend, DispatchBackend::AppServer);
        assert_ne!(plan.backend, DispatchBackend::ResponsesFallback);
    }
}
