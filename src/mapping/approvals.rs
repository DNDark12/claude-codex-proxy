use crate::app_server::events::{AppServerEvent, AppServerEventKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxConfig {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConfigRequirements {
    pub allowed_approval_policies: Option<Vec<ApprovalPolicy>>,
    pub allowed_sandbox_modes: Option<Vec<SandboxConfig>>,
    pub allowed_web_search_modes: Option<Vec<String>>,
    pub enforce_residency: Option<String>,
}

pub fn resolve_approval_policy(
    thread_policy: ApprovalPolicy,
    turn_override: Option<ApprovalPolicy>,
    requirements: Option<&ConfigRequirements>,
) -> Result<ApprovalPolicy, String> {
    let effective = turn_override.unwrap_or(thread_policy);
    if approval_rank(effective) < approval_rank(thread_policy) {
        return Err(format!(
            "turn approval policy {effective:?} is looser than thread policy {thread_policy:?}"
        ));
    }

    if let Some(allowed) = requirements.and_then(|config| config.allowed_approval_policies.as_ref())
    {
        if !allowed.contains(&effective) {
            return Err(format!(
                "approval policy {effective:?} is not allowed by app-server"
            ));
        }
    }

    Ok(effective)
}

pub fn resolve_sandbox_config(
    thread_sandbox: SandboxConfig,
    turn_override: Option<SandboxConfig>,
    requirements: Option<&ConfigRequirements>,
) -> Result<SandboxConfig, String> {
    let effective = turn_override.unwrap_or(thread_sandbox);
    if sandbox_rank(effective) < sandbox_rank(thread_sandbox) {
        return Err(format!(
            "turn sandbox {effective:?} is looser than thread sandbox {thread_sandbox:?}"
        ));
    }

    if let Some(allowed) = requirements.and_then(|config| config.allowed_sandbox_modes.as_ref()) {
        if !allowed.contains(&effective) {
            return Err(format!(
                "sandbox mode {effective:?} is not allowed by app-server"
            ));
        }
    }

    Ok(effective)
}

fn approval_rank(policy: ApprovalPolicy) -> usize {
    match policy {
        ApprovalPolicy::Never => 0,
        ApprovalPolicy::OnFailure => 1,
        ApprovalPolicy::OnRequest => 2,
        ApprovalPolicy::Untrusted => 3,
    }
}

fn sandbox_rank(config: SandboxConfig) -> usize {
    match config {
        SandboxConfig::DangerFullAccess => 0,
        SandboxConfig::WorkspaceWrite => 1,
        SandboxConfig::ReadOnly => 2,
    }
}

/// Detect if an app-server event signals an approval pause (P1-022).
pub fn detect_approval_pause(event: &AppServerEvent) -> Option<ApprovalPauseInfo> {
    if event.kind == AppServerEventKind::TerminalInteraction {
        if let Some(action) = event.params.get("action").and_then(|v| v.as_str()) {
            if action == "approval_request" || action == "approvalRequest" {
                return Some(ApprovalPauseInfo {
                    thread_id: event.thread_id.clone().unwrap_or_default(),
                    turn_id: event.turn_id.clone().unwrap_or_default(),
                    action_description: event
                        .params
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Action requires approval")
                        .to_string(),
                    tool_name: event
                        .params
                        .get("toolName")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                });
            }
        }
    }
    // Also detect via turn status in turn events
    if event.kind == AppServerEventKind::TurnStarted
        || event.kind == AppServerEventKind::TurnCompleted
    {
        if let Some(status) = event
            .params
            .get("status")
            .or_else(|| event.params.get("turn").and_then(|turn| turn.get("status")))
            .and_then(|v| v.as_str())
        {
            if status == "waitingOnApproval" || status == "waiting_on_approval" {
                return Some(ApprovalPauseInfo {
                    thread_id: event.thread_id.clone().unwrap_or_default(),
                    turn_id: event.turn_id.clone().unwrap_or_default(),
                    action_description: "Turn paused waiting for approval".to_string(),
                    tool_name: None,
                });
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalPauseInfo {
    pub thread_id: String,
    pub turn_id: String,
    pub action_description: String,
    pub tool_name: Option<String>,
}

/// Approval response from client (P1-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResponse {
    Allow,
    Deny,
    AllowAlways,
}

impl ApprovalResponse {
    /// Convert to the JSON value app-server expects for approval reply.
    pub fn to_server_value(self) -> serde_json::Value {
        self.to_server_value_for_method("item/commandExecution/requestApproval")
    }

    pub fn to_server_value_for_method(self, method: &str) -> serde_json::Value {
        let decision = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                match self {
                    Self::Allow => "accept",
                    Self::Deny => "decline",
                    Self::AllowAlways => "acceptForSession",
                }
            }
            "applyPatchApproval" | "execCommandApproval" => match self {
                Self::Allow => "approved",
                Self::Deny => "denied",
                Self::AllowAlways => "approved_for_session",
            },
            _ => match self {
                Self::Allow => "accept",
                Self::Deny => "decline",
                Self::AllowAlways => "acceptForSession",
            },
        };

        serde_json::json!({ "decision": decision })
    }
}

/// Map Claude sandbox intent to thread/start parameters (P1-024).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxIntent {
    pub approval_policy: ApprovalPolicy,
    pub sandbox_config: SandboxConfig,
}

pub fn translate_sandbox_intent(
    claude_sandbox_mode: Option<&str>,
    requirements: Option<&ConfigRequirements>,
) -> SandboxIntent {
    let (policy, sandbox) = match claude_sandbox_mode {
        Some("strict" | "locked") => (ApprovalPolicy::Untrusted, SandboxConfig::ReadOnly),
        Some("standard" | "default") => (ApprovalPolicy::OnRequest, SandboxConfig::WorkspaceWrite),
        Some("permissive" | "full") => (ApprovalPolicy::OnFailure, SandboxConfig::DangerFullAccess),
        _ => (ApprovalPolicy::OnRequest, SandboxConfig::WorkspaceWrite),
    };

    // Clamp to allowed if requirements restrict
    let policy =
        if let Some(allowed) = requirements.and_then(|r| r.allowed_approval_policies.as_ref()) {
            if allowed.contains(&policy) {
                policy
            } else {
                // Fall back to most restrictive allowed
                *allowed
                    .iter()
                    .max_by_key(|p| approval_rank(**p))
                    .unwrap_or(&ApprovalPolicy::Untrusted)
            }
        } else {
            policy
        };

    let sandbox = if let Some(allowed) = requirements.and_then(|r| r.allowed_sandbox_modes.as_ref())
    {
        if allowed.contains(&sandbox) {
            sandbox
        } else {
            *allowed
                .iter()
                .max_by_key(|s| sandbox_rank(**s))
                .unwrap_or(&SandboxConfig::ReadOnly)
        }
    } else {
        sandbox
    };

    SandboxIntent {
        approval_policy: policy,
        sandbox_config: sandbox,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_looser_turn_policy() {
        let err =
            resolve_approval_policy(ApprovalPolicy::OnRequest, Some(ApprovalPolicy::Never), None)
                .expect_err("should reject");
        assert!(err.contains("looser"));
    }

    #[test]
    fn rejects_looser_turn_sandbox() {
        let err = resolve_sandbox_config(
            SandboxConfig::ReadOnly,
            Some(SandboxConfig::WorkspaceWrite),
            None,
        )
        .expect_err("should reject");
        assert!(err.contains("looser"));
    }

    // P1-T04: turn-level override rejected when looser than thread policy
    #[test]
    fn accepts_tighter_turn_policy() {
        let result = resolve_approval_policy(
            ApprovalPolicy::OnRequest,
            Some(ApprovalPolicy::Untrusted),
            None,
        );
        assert_eq!(result.unwrap(), ApprovalPolicy::Untrusted);
    }

    #[test]
    fn rejects_policy_not_in_requirements() {
        let reqs = ConfigRequirements {
            allowed_approval_policies: Some(vec![ApprovalPolicy::OnRequest]),
            ..Default::default()
        };
        let err = resolve_approval_policy(
            ApprovalPolicy::OnRequest,
            Some(ApprovalPolicy::Untrusted),
            Some(&reqs),
        )
        .expect_err("should reject");
        assert!(err.contains("not allowed"));
    }

    // P1-024: sandbox intent translation
    #[test]
    fn sandbox_intent_strict_mode() {
        let intent = translate_sandbox_intent(Some("strict"), None);
        assert_eq!(intent.approval_policy, ApprovalPolicy::Untrusted);
        assert_eq!(intent.sandbox_config, SandboxConfig::ReadOnly);
    }

    #[test]
    fn sandbox_intent_default_mode() {
        let intent = translate_sandbox_intent(None, None);
        assert_eq!(intent.approval_policy, ApprovalPolicy::OnRequest);
        assert_eq!(intent.sandbox_config, SandboxConfig::WorkspaceWrite);
    }

    #[test]
    fn sandbox_intent_clamps_to_requirements() {
        let reqs = ConfigRequirements {
            allowed_approval_policies: Some(vec![ApprovalPolicy::Untrusted]),
            allowed_sandbox_modes: Some(vec![SandboxConfig::ReadOnly]),
            ..Default::default()
        };
        let intent = translate_sandbox_intent(Some("permissive"), Some(&reqs));
        assert_eq!(intent.approval_policy, ApprovalPolicy::Untrusted);
        assert_eq!(intent.sandbox_config, SandboxConfig::ReadOnly);
    }

    // P1-022: detect approval pause from event
    #[test]
    fn detects_approval_pause_from_terminal_interaction() {
        let event = AppServerEvent {
            method: "terminal_interaction".to_string(),
            kind: AppServerEventKind::TerminalInteraction,
            params: serde_json::json!({
                "action": "approval_request",
                "description": "Write to file",
                "toolName": "Write",
            }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("turn1".to_string()),
            item_id: None,
            delta: None,
        };
        let pause = detect_approval_pause(&event).expect("should detect");
        assert_eq!(pause.thread_id, "t1");
        assert_eq!(pause.action_description, "Write to file");
        assert_eq!(pause.tool_name.as_deref(), Some("Write"));
    }

    #[test]
    fn no_approval_pause_for_clarification() {
        let event = AppServerEvent {
            method: "terminal_interaction".to_string(),
            kind: AppServerEventKind::TerminalInteraction,
            params: serde_json::json!({ "action": "ask_user", "question": "Which file?" }),
            thread_id: Some("t1".to_string()),
            turn_id: Some("turn1".to_string()),
            item_id: None,
            delta: None,
        };
        assert!(detect_approval_pause(&event).is_none());
    }

    // P1-023: approval response values
    #[test]
    fn approval_response_to_server_value() {
        let v = ApprovalResponse::Allow.to_server_value();
        assert_eq!(v["decision"], "accept");
        let v = ApprovalResponse::Deny.to_server_value();
        assert_eq!(v["decision"], "decline");
        let v = ApprovalResponse::AllowAlways.to_server_value_for_method("execCommandApproval");
        assert_eq!(v["decision"], "approved_for_session");
    }
}
