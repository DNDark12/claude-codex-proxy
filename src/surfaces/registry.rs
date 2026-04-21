use std::collections::HashMap;

use super::model::{
    ApprovalSensitivity, AsyncMode, AvailabilityGate, FallbackMode, HostDependency, InvocationMode,
    MappingStrategy, SideEffectLevel, StateScope, SurfaceBucket, SurfaceDescriptor, SurfaceFamily,
    SurfaceKind,
};

#[derive(Debug, Clone)]
pub struct SurfaceRegistry {
    surfaces: Vec<SurfaceDescriptor>,
    by_id: HashMap<String, usize>,
    by_source_name: HashMap<String, usize>,
}

struct ToolSemantics {
    strategy: MappingStrategy,
    fallback_mode: FallbackMode,
    side_effect_level: SideEffectLevel,
    availability_gate: AvailabilityGate,
}

impl SurfaceRegistry {
    pub fn new() -> Self {
        let surfaces = build_registry();
        let by_id = surfaces
            .iter()
            .enumerate()
            .map(|(idx, surface)| (surface.id.clone(), idx))
            .collect();
        let by_source_name = surfaces
            .iter()
            .enumerate()
            .map(|(idx, surface)| (surface.source_name.to_ascii_lowercase(), idx))
            .collect();

        Self {
            surfaces,
            by_id,
            by_source_name,
        }
    }

    pub fn all(&self) -> &[SurfaceDescriptor] {
        &self.surfaces
    }

    pub fn get(&self, id: &str) -> Option<&SurfaceDescriptor> {
        self.by_id.get(id).and_then(|idx| self.surfaces.get(*idx))
    }

    pub fn find_by_source_name(&self, source_name: &str) -> Option<&SurfaceDescriptor> {
        self.by_source_name
            .get(&source_name.to_ascii_lowercase())
            .and_then(|idx| self.surfaces.get(*idx))
    }
}

impl Default for SurfaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn build_registry() -> Vec<SurfaceDescriptor> {
    vec![
        tool(
            "tool.read",
            "Read",
            SurfaceFamily::FileCode,
            0,
            MappingStrategy::Native,
            FallbackMode::HardError,
            SideEffectLevel::None,
        ),
        tool(
            "tool.write",
            "Write",
            SurfaceFamily::FileCode,
            0,
            MappingStrategy::MediatedNative,
            FallbackMode::HardError,
            SideEffectLevel::LocalWrite,
        ),
        tool(
            "tool.edit",
            "Edit",
            SurfaceFamily::FileCode,
            0,
            MappingStrategy::MediatedNative,
            FallbackMode::HardError,
            SideEffectLevel::LocalWrite,
        ),
        tool(
            "tool.multiedit",
            "MultiEdit",
            SurfaceFamily::FileCode,
            0,
            MappingStrategy::MediatedNative,
            FallbackMode::SoftWarningAndContinue,
            SideEffectLevel::LocalWrite,
        ),
        tool(
            "tool.glob",
            "Glob",
            SurfaceFamily::FileCode,
            0,
            MappingStrategy::Native,
            FallbackMode::HardError,
            SideEffectLevel::None,
        ),
        tool(
            "tool.grep",
            "Grep",
            SurfaceFamily::FileCode,
            0,
            MappingStrategy::Native,
            FallbackMode::HardError,
            SideEffectLevel::None,
        ),
        tool(
            "tool.ls",
            "LS",
            SurfaceFamily::FileCode,
            0,
            MappingStrategy::Native,
            FallbackMode::HardError,
            SideEffectLevel::None,
        ),
        tool(
            "tool.bash",
            "Bash",
            SurfaceFamily::Execution,
            0,
            MappingStrategy::MediatedNative,
            FallbackMode::HardError,
            SideEffectLevel::ShellExec,
        ),
        tool(
            "tool.taskcreate",
            "TaskCreate",
            SurfaceFamily::Jobs,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.taskget",
            "TaskGet",
            SurfaceFamily::Jobs,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::None,
        ),
        tool(
            "tool.tasklist",
            "TaskList",
            SurfaceFamily::Jobs,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::None,
        ),
        tool(
            "tool.taskupdate",
            "TaskUpdate",
            SurfaceFamily::Jobs,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.taskstop",
            "TaskStop",
            SurfaceFamily::Jobs,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.agent",
            "Agent",
            SurfaceFamily::Subagents,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::StateMutation,
        ),
        gated_tool(
            "tool.sendmessage",
            "SendMessage",
            SurfaceFamily::Subagents,
            1,
            ToolSemantics {
                strategy: MappingStrategy::MediatedNative,
                fallback_mode: FallbackMode::DowngradeToWorkflow,
                side_effect_level: SideEffectLevel::StateMutation,
                availability_gate: AvailabilityGate {
                    env_flags: vec!["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS".to_string()],
                    ..AvailabilityGate::default()
                },
            },
        ),
        tool(
            "tool.askuserquestion",
            "AskUserQuestion",
            SurfaceFamily::Interaction,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::HardError,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.enterplanmode",
            "EnterPlanMode",
            SurfaceFamily::Planning,
            2,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.exitplanmode",
            "ExitPlanMode",
            SurfaceFamily::Planning,
            2,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.enterworktree",
            "EnterWorktree",
            SurfaceFamily::Workspace,
            2,
            MappingStrategy::MediatedNative,
            FallbackMode::HardError,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.exitworktree",
            "ExitWorktree",
            SurfaceFamily::Workspace,
            2,
            MappingStrategy::MediatedNative,
            FallbackMode::HardError,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.croncreate",
            "CronCreate",
            SurfaceFamily::Scheduling,
            3,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::StateMutation,
        ),
        tool(
            "tool.cronlist",
            "CronList",
            SurfaceFamily::Scheduling,
            3,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::None,
        ),
        tool(
            "tool.crondelete",
            "CronDelete",
            SurfaceFamily::Scheduling,
            3,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
            SideEffectLevel::StateMutation,
        ),
        gated_tool(
            "tool.monitor",
            "Monitor",
            SurfaceFamily::Observability,
            3,
            ToolSemantics {
                strategy: MappingStrategy::WorkflowEmulated,
                fallback_mode: FallbackMode::SoftWarningAndContinue,
                side_effect_level: SideEffectLevel::None,
                availability_gate: AvailabilityGate {
                    min_version: Some("2.1.98".to_string()),
                    plan_or_product: Some("Unavailable on Bedrock/Vertex/Foundry".to_string()),
                    ..AvailabilityGate::default()
                },
            },
        ),
        gated_tool(
            "tool.lsp",
            "LSP",
            SurfaceFamily::CodeIntelligence,
            3,
            ToolSemantics {
                strategy: MappingStrategy::UnsupportedExplicit,
                fallback_mode: FallbackMode::HardError,
                side_effect_level: SideEffectLevel::None,
                availability_gate: AvailabilityGate {
                    required_plugins: vec!["code-intelligence".to_string()],
                    required_binaries: vec!["clangd".to_string()],
                    platform: Some(std::env::consts::OS.to_string()),
                    ..AvailabilityGate::default()
                },
            },
        ),
        tool(
            "tool.toolsearch",
            "ToolSearch",
            SurfaceFamily::Meta,
            3,
            MappingStrategy::MediatedNative,
            FallbackMode::SoftWarningAndContinue,
            SideEffectLevel::None,
        ),
        tool(
            "tool.webfetch",
            "WebFetch",
            SurfaceFamily::SearchWeb,
            3,
            MappingStrategy::MediatedNative,
            FallbackMode::SoftWarningAndContinue,
            SideEffectLevel::Network,
        ),
        tool(
            "tool.websearch",
            "WebSearch",
            SurfaceFamily::SearchWeb,
            3,
            MappingStrategy::MediatedNative,
            FallbackMode::SoftWarningAndContinue,
            SideEffectLevel::Network,
        ),
        tool(
            "tool.notebookread",
            "NotebookRead",
            SurfaceFamily::Notebook,
            4,
            MappingStrategy::MediatedNative,
            FallbackMode::SoftWarningAndContinue,
            SideEffectLevel::None,
        ),
        tool(
            "tool.notebookedit",
            "NotebookEdit",
            SurfaceFamily::Notebook,
            4,
            MappingStrategy::UnsupportedExplicit,
            FallbackMode::HardError,
            SideEffectLevel::LocalWrite,
        ),
        out_of_scope_tool("tool.todowrite", "TodoWrite", SurfaceFamily::Jobs),
        platform_tool(
            "tool.powershell",
            "PowerShell",
            SurfaceFamily::Execution,
            Some("windows"),
        ),
        out_of_scope_tool("tool.teamcreate", "TeamCreate", SurfaceFamily::Teams),
        out_of_scope_tool("tool.teamdelete", "TeamDelete", SurfaceFamily::Teams),
        command(
            "command.tasks",
            "/tasks",
            SurfaceFamily::Jobs,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
        ),
        command(
            "command.security_review",
            "/security-review",
            SurfaceFamily::Review,
            1,
            MappingStrategy::WorkflowEmulated,
            FallbackMode::DowngradeToWorkflow,
        ),
        command(
            "command.sandbox",
            "/sandbox",
            SurfaceFamily::ConfigPermissions,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::HardError,
        ),
        command(
            "command.plan",
            "/plan",
            SurfaceFamily::Planning,
            2,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
        ),
        command(
            "command.resume",
            "/resume",
            SurfaceFamily::Workspace,
            2,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
        ),
        command(
            "command.rewind",
            "/rewind",
            SurfaceFamily::Workspace,
            2,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
        ),
        command(
            "command.permissions",
            "/permissions",
            SurfaceFamily::ConfigPermissions,
            2,
            MappingStrategy::MediatedNative,
            FallbackMode::HardError,
        ),
        command(
            "command.schedule",
            "/schedule",
            SurfaceFamily::DurableRoutines,
            3,
            MappingStrategy::UnsupportedExplicit,
            FallbackMode::HardError,
        ),
        command(
            "command.init",
            "/init",
            SurfaceFamily::GuidanceMemory,
            4,
            MappingStrategy::WorkflowEmulated,
            FallbackMode::DowngradeToWorkflow,
        ),
        command(
            "command.memory",
            "/memory",
            SurfaceFamily::GuidanceMemory,
            4,
            MappingStrategy::WorkflowEmulated,
            FallbackMode::DowngradeToWorkflow,
        ),
        command(
            "command.mcp",
            "/mcp",
            SurfaceFamily::Mcp,
            4,
            MappingStrategy::MediatedNative,
            FallbackMode::SoftWarningAndContinue,
        ),
        command(
            "command.plugin",
            "/plugin",
            SurfaceFamily::Skills,
            4,
            MappingStrategy::WorkflowEmulated,
            FallbackMode::DowngradeToWorkflow,
        ),
        host_admin_command("command.doctor", "/doctor"),
        host_admin_command("command.help", "/help"),
        host_admin_command("command.theme", "/theme"),
        host_admin_command("command.vim", "/vim"),
        host_admin_command("command.login", "/login"),
        host_admin_command("command.logout", "/logout"),
        platform_command("command.remote_control", "/remote-control"),
        platform_command("command.teleport", "/teleport"),
        platform_command("command.desktop", "/desktop"),
        workflow(
            "workflow.code_review",
            "code_review",
            SurfaceFamily::Review,
            1,
            MappingStrategy::WorkflowEmulated,
            FallbackMode::DowngradeToWorkflow,
        ),
        workflow(
            "workflow.security_review",
            "security_review",
            SurfaceFamily::Review,
            1,
            MappingStrategy::WorkflowEmulated,
            FallbackMode::DowngradeToWorkflow,
        ),
        workflow(
            "workflow.rescue_fix",
            "rescue_fix",
            SurfaceFamily::Review,
            1,
            MappingStrategy::WorkflowEmulated,
            FallbackMode::DowngradeToWorkflow,
        ),
        workflow(
            "workflow.review_status",
            "review_status",
            SurfaceFamily::Review,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
        ),
        workflow(
            "workflow.review_cancel",
            "review_cancel",
            SurfaceFamily::Review,
            1,
            MappingStrategy::MediatedNative,
            FallbackMode::DowngradeToWorkflow,
        ),
    ]
}

fn tool(
    id: &str,
    source_name: &str,
    family: SurfaceFamily,
    tier: u8,
    strategy: MappingStrategy,
    fallback_mode: FallbackMode,
    side_effect_level: SideEffectLevel,
) -> SurfaceDescriptor {
    gated_tool(
        id,
        source_name,
        family,
        tier,
        ToolSemantics {
            strategy,
            fallback_mode,
            side_effect_level,
            availability_gate: AvailabilityGate::default(),
        },
    )
}

fn gated_tool(
    id: &str,
    source_name: &str,
    family: SurfaceFamily,
    tier: u8,
    semantics: ToolSemantics,
) -> SurfaceDescriptor {
    let ToolSemantics {
        strategy,
        fallback_mode,
        side_effect_level,
        availability_gate,
    } = semantics;
    SurfaceDescriptor {
        id: id.to_string(),
        source_provider: "claude_code".to_string(),
        source_name: source_name.to_string(),
        surface_kind: SurfaceKind::Tool,
        family,
        bucket: match family {
            SurfaceFamily::CodeIntelligence => SurfaceBucket::PlatformSpecific,
            _ => SurfaceBucket::WorkflowRuntime,
        },
        invocation_mode: InvocationMode::ModelInvoked,
        state_scope: StateScope::Thread,
        side_effect_level,
        async_mode: if tier > 0 {
            AsyncMode::Async
        } else {
            AsyncMode::Sync
        },
        approval_sensitivity: match side_effect_level {
            SideEffectLevel::LocalWrite | SideEffectLevel::ShellExec | SideEffectLevel::Network => {
                ApprovalSensitivity::Strict
            }
            SideEffectLevel::StateMutation => ApprovalSensitivity::Ask,
            SideEffectLevel::None => ApprovalSensitivity::None,
        },
        host_dependency: match family {
            SurfaceFamily::FileCode | SurfaceFamily::Workspace | SurfaceFamily::Notebook => {
                HostDependency::LocalFs
            }
            SurfaceFamily::Execution | SurfaceFamily::CodeIntelligence => HostDependency::Cli,
            SurfaceFamily::SearchWeb => HostDependency::App,
            _ => HostDependency::None,
        },
        tier,
        availability_gate,
        strategy,
        fallback_mode,
    }
}

fn platform_tool(
    id: &str,
    source_name: &str,
    family: SurfaceFamily,
    platform: Option<&str>,
) -> SurfaceDescriptor {
    SurfaceDescriptor {
        bucket: SurfaceBucket::PlatformSpecific,
        availability_gate: AvailabilityGate {
            platform: platform.map(str::to_string),
            ..AvailabilityGate::default()
        },
        strategy: MappingStrategy::UnsupportedExplicit,
        fallback_mode: FallbackMode::HardError,
        ..tool(
            id,
            source_name,
            family,
            5,
            MappingStrategy::UnsupportedExplicit,
            FallbackMode::HardError,
            SideEffectLevel::ShellExec,
        )
    }
}

fn out_of_scope_tool(id: &str, source_name: &str, family: SurfaceFamily) -> SurfaceDescriptor {
    SurfaceDescriptor {
        bucket: SurfaceBucket::OutOfScope,
        strategy: MappingStrategy::UnsupportedExplicit,
        fallback_mode: FallbackMode::DropWithObservability,
        tier: 4,
        ..tool(
            id,
            source_name,
            family,
            4,
            MappingStrategy::UnsupportedExplicit,
            FallbackMode::DropWithObservability,
            SideEffectLevel::StateMutation,
        )
    }
}

fn command(
    id: &str,
    source_name: &str,
    family: SurfaceFamily,
    tier: u8,
    strategy: MappingStrategy,
    fallback_mode: FallbackMode,
) -> SurfaceDescriptor {
    SurfaceDescriptor {
        id: id.to_string(),
        source_provider: "claude_code".to_string(),
        source_name: source_name.to_string(),
        surface_kind: SurfaceKind::Command,
        family,
        bucket: if matches!(family, SurfaceFamily::ConfigPermissions) {
            SurfaceBucket::RuntimeCritical
        } else {
            SurfaceBucket::WorkflowRuntime
        },
        invocation_mode: InvocationMode::UserCommand,
        state_scope: StateScope::Thread,
        side_effect_level: SideEffectLevel::StateMutation,
        async_mode: AsyncMode::Async,
        approval_sensitivity: ApprovalSensitivity::Ask,
        host_dependency: HostDependency::Cli,
        tier,
        availability_gate: AvailabilityGate::default(),
        strategy,
        fallback_mode,
    }
}

fn host_admin_command(id: &str, source_name: &str) -> SurfaceDescriptor {
    SurfaceDescriptor {
        bucket: SurfaceBucket::HostAdminUx,
        strategy: MappingStrategy::UnsupportedExplicit,
        fallback_mode: FallbackMode::DropWithObservability,
        tier: 5,
        ..command(
            id,
            source_name,
            SurfaceFamily::UiMisc,
            5,
            MappingStrategy::UnsupportedExplicit,
            FallbackMode::DropWithObservability,
        )
    }
}

fn platform_command(id: &str, source_name: &str) -> SurfaceDescriptor {
    SurfaceDescriptor {
        bucket: SurfaceBucket::PlatformSpecific,
        strategy: MappingStrategy::UnsupportedExplicit,
        fallback_mode: FallbackMode::HardError,
        tier: 5,
        ..command(
            id,
            source_name,
            SurfaceFamily::UiMisc,
            5,
            MappingStrategy::UnsupportedExplicit,
            FallbackMode::HardError,
        )
    }
}

fn workflow(
    id: &str,
    source_name: &str,
    family: SurfaceFamily,
    tier: u8,
    strategy: MappingStrategy,
    fallback_mode: FallbackMode,
) -> SurfaceDescriptor {
    SurfaceDescriptor {
        id: id.to_string(),
        source_provider: "claude_code".to_string(),
        source_name: source_name.to_string(),
        surface_kind: SurfaceKind::Workflow,
        family,
        bucket: SurfaceBucket::WorkflowRuntime,
        invocation_mode: InvocationMode::Background,
        state_scope: StateScope::Job,
        side_effect_level: SideEffectLevel::StateMutation,
        async_mode: AsyncMode::Background,
        approval_sensitivity: ApprovalSensitivity::Ask,
        host_dependency: HostDependency::Cli,
        tier,
        availability_gate: AvailabilityGate::default(),
        strategy,
        fallback_mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_required_surfaces() {
        let registry = SurfaceRegistry::new();
        assert!(registry.find_by_source_name("Read").is_some());
        assert!(registry.find_by_source_name("/plan").is_some());
        assert!(registry.find_by_source_name("TaskCreate").is_some());
    }

    #[test]
    fn every_registered_surface_has_core_policy_fields() {
        let registry = SurfaceRegistry::new();
        for surface in registry.all() {
            assert!(!surface.id.is_empty(), "missing id");
            assert!(!surface.source_name.is_empty(), "missing source_name");
            assert!(surface.tier <= 5, "invalid tier");
        }
    }
}
