use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    Tool,
    Command,
    Skill,
    Workflow,
    StateSurface,
    HostIntegration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceFamily {
    FileCode,
    Execution,
    SearchWeb,
    Review,
    Jobs,
    Planning,
    Workspace,
    Scheduling,
    DurableRoutines,
    GuidanceMemory,
    ConfigPermissions,
    Mcp,
    Subagents,
    CodeIntelligence,
    Interaction,
    Teams,
    Meta,
    Observability,
    Notebook,
    UiMisc,
    Skills,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceBucket {
    RuntimeCritical,
    WorkflowRuntime,
    HostAdminUx,
    PlatformSpecific,
    OutOfScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingStrategy {
    Native,
    MediatedNative,
    WorkflowEmulated,
    UnsupportedExplicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackMode {
    HardError,
    SoftWarningAndContinue,
    DowngradeToWorkflow,
    DropWithObservability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationMode {
    StrictAppServer,
    AutoHybrid,
    ResponsesOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateScope {
    Stateless,
    Request,
    Turn,
    Thread,
    Workspace,
    ProjectConfig,
    UserConfig,
    Job,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectLevel {
    None,
    LocalWrite,
    ShellExec,
    Network,
    StateMutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncMode {
    Sync,
    Async,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSensitivity {
    None,
    Ask,
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostDependency {
    None,
    LocalFs,
    Cli,
    Mcp,
    App,
    PlatformSpecific,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationMode {
    ModelInvoked,
    UserCommand,
    Implicit,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AvailabilityGate {
    pub min_version: Option<String>,
    #[serde(default)]
    pub env_flags: Vec<String>,
    #[serde(default)]
    pub required_plugins: Vec<String>,
    #[serde(default)]
    pub required_binaries: Vec<String>,
    pub platform: Option<String>,
    pub plan_or_product: Option<String>,
    #[serde(default)]
    pub experimental: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceDescriptor {
    pub id: String,
    pub source_provider: String,
    pub source_name: String,
    pub surface_kind: SurfaceKind,
    pub family: SurfaceFamily,
    pub bucket: SurfaceBucket,
    pub invocation_mode: InvocationMode,
    pub state_scope: StateScope,
    pub side_effect_level: SideEffectLevel,
    pub async_mode: AsyncMode,
    pub approval_sensitivity: ApprovalSensitivity,
    pub host_dependency: HostDependency,
    pub tier: u8,
    pub availability_gate: AvailabilityGate,
    pub strategy: MappingStrategy,
    pub fallback_mode: FallbackMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedReason {
    MissingBackendPrimitive,
    StateModelMismatch,
    ApprovalModelMismatch,
    DeprecatedSourceSurface,
    HostDependencyGap,
    PlatformSpecificGap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingDecision {
    pub surface_id: String,
    pub target_backend: String,
    pub target_surface: Option<String>,
    pub strategy: MappingStrategy,
    pub fallback_mode: FallbackMode,
    pub requires_mode: OperationMode,
    pub unsupported_reason: Option<UnsupportedReason>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl AvailabilityGate {
    pub fn is_satisfied(&self) -> bool {
        let os = std::env::consts::OS;
        let platform_ok = self
            .platform
            .as_ref()
            .map(|platform| platform.eq_ignore_ascii_case(os))
            .unwrap_or(true);
        let env_ok = self
            .env_flags
            .iter()
            .all(|flag| std::env::var(flag).ok().filter(|v| !v.is_empty()).is_some());
        let binaries_ok = self
            .required_binaries
            .iter()
            .all(|binary| binary_in_path(binary));
        platform_ok && env_ok && binaries_ok
    }
}

fn binary_in_path(binary: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&path_var).any(|entry| entry.join(binary).exists())
}
