pub mod doctor;
pub mod env;
pub mod setup;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use tokio::process::Command;

use crate::app_server::{ApiStability, AppServerClient, AppServerConnectOptions, AuthStatus};
use crate::mapping::approvals::ConfigRequirements;
use crate::model_profiles::expand_public_models;
use crate::proxy::codex_client::CodexClient;
use crate::surfaces::{CompatibilityMatrix, OperationMode, SurfaceRegistry};

#[derive(Debug, Clone, Args)]
pub struct RuntimeArgs {
    #[arg(short, long)]
    pub port: Option<u16>,

    #[arg(long)]
    pub auth_path: Option<String>,

    #[arg(long)]
    pub skills_registry_path: Option<String>,

    #[arg(long, value_enum, default_value_t = CliOperationMode::AutoHybrid)]
    pub mode: CliOperationMode,

    #[arg(
        long,
        default_value_t = false,
        conflicts_with = "app_server_experimental"
    )]
    pub app_server_stable: bool,

    #[arg(long, default_value_t = false)]
    pub app_server_experimental: bool,

    #[arg(long, value_enum, default_value_t = CliDelegationPolicy::ExplicitOnly)]
    pub delegation_policy: CliDelegationPolicy,
}

#[derive(Debug, Clone, Args)]
pub struct SetupArgs {
    #[command(flatten)]
    pub runtime: RuntimeArgs,

    #[arg(long, default_value_t = false)]
    pub write_config: bool,
}

#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    #[command(flatten)]
    pub runtime: RuntimeArgs,

    #[arg(long, default_value_t = false)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct EnvArgs {
    #[command(flatten)]
    pub runtime: RuntimeArgs,

    #[arg(long, value_enum)]
    pub shell: Option<EnvShell>,

    #[arg(long)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    Serve(RuntimeArgs),
    Setup(SetupArgs),
    Doctor(DoctorArgs),
    Env(EnvArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliOperationMode {
    StrictAppServer,
    AutoHybrid,
    ResponsesOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliDelegationPolicy {
    Never,
    ExplicitOnly,
    Heuristic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EnvShell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexBinaryInfo {
    pub path: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppServerProbe {
    pub available: bool,
    pub user_agent: Option<String>,
    pub api_stability: ApiStability,
    pub requirements: Option<ConfigRequirements>,
    pub auth: Option<AuthStatus>,
    pub command_exec_ok: bool,
    pub models: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub port: u16,
    pub auth_path: String,
    pub skills_registry_path: Option<String>,
    pub operation_mode: OperationMode,
    pub api_stability: ApiStability,
    pub delegation_policy: crate::app_server::DelegationPolicy,
}

impl RuntimeArgs {
    pub fn resolve(&self) -> RuntimeConfig {
        RuntimeConfig {
            port: resolve_port(self.port),
            auth_path: resolve_auth_path(self.auth_path.clone()),
            skills_registry_path: resolve_skills_registry_path(self.skills_registry_path.clone()),
            operation_mode: match self.mode {
                CliOperationMode::StrictAppServer => OperationMode::StrictAppServer,
                CliOperationMode::AutoHybrid => OperationMode::AutoHybrid,
                CliOperationMode::ResponsesOnly => OperationMode::ResponsesOnly,
            },
            api_stability: if self.app_server_experimental {
                ApiStability::Experimental
            } else {
                ApiStability::Stable
            },
            delegation_policy: match self.delegation_policy {
                CliDelegationPolicy::Never => crate::app_server::DelegationPolicy::Never,
                CliDelegationPolicy::ExplicitOnly => {
                    crate::app_server::DelegationPolicy::ExplicitOnly
                }
                CliDelegationPolicy::Heuristic => crate::app_server::DelegationPolicy::Heuristic,
            },
        }
    }
}

pub async fn detect_codex_binary() -> CodexBinaryInfo {
    let path = find_binary_in_path("codex");
    let version = version_from_binary(path.as_deref().unwrap_or("codex"))
        .await
        .ok();
    CodexBinaryInfo { path, version }
}

pub async fn probe_app_server(runtime: &RuntimeConfig) -> AppServerProbe {
    if matches!(runtime.operation_mode, OperationMode::ResponsesOnly) {
        return AppServerProbe {
            available: false,
            user_agent: None,
            api_stability: runtime.api_stability,
            requirements: None,
            auth: None,
            command_exec_ok: false,
            models: Vec::new(),
            error: Some("responses-only mode".to_string()),
        };
    }

    let client = AppServerClient::connect(AppServerConnectOptions {
        api_stability: runtime.api_stability,
        ..AppServerConnectOptions::default()
    })
    .await;

    match client {
        Ok(client) => {
            let auth = client.auth_status().await.ok();
            let requirements = client.config_requirements().cloned();
            let models = client
                .model_list()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>();
            let command_exec_ok = client
                .command_exec(
                    vec!["pwd".to_string()],
                    Some(
                        std::env::current_dir()
                            .ok()
                            .map(|dir| dir.display().to_string())
                            .unwrap_or_else(|| ".".to_string()),
                    ),
                    Some(5_000),
                )
                .await
                .is_ok();
            let user_agent = Some(client.handshake().user_agent.clone());
            let _ = client.kill().await;
            AppServerProbe {
                available: true,
                user_agent,
                api_stability: runtime.api_stability,
                requirements,
                auth,
                command_exec_ok,
                models: expand_public_models(models),
                error: None,
            }
        }
        Err(error) => AppServerProbe {
            available: false,
            user_agent: None,
            api_stability: runtime.api_stability,
            requirements: None,
            auth: None,
            command_exec_ok: false,
            models: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

pub async fn discover_public_models(runtime: &RuntimeConfig) -> Vec<String> {
    let mut models = Vec::new();

    if !matches!(runtime.operation_mode, OperationMode::ResponsesOnly) {
        models.extend(probe_app_server(runtime).await.models);
    }

    if matches!(
        runtime.operation_mode,
        OperationMode::AutoHybrid | OperationMode::ResponsesOnly
    ) {
        if let Ok(client) = CodexClient::from_auth_path(&runtime.auth_path).await {
            models.extend(client.list_models().await);
        }
    }

    if models.is_empty() {
        models.push("gpt-5.2-codex".to_string());
    }

    expand_public_models(models)
}

pub fn degraded_surfaces(
    registry: &SurfaceRegistry,
    matrix: &CompatibilityMatrix,
    mode: OperationMode,
) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for surface in registry.all() {
        let decision = matrix.get(&surface.id, mode).expect("matrix decision");
        let mut reasons = Vec::new();
        if !surface.availability_gate.is_satisfied() {
            reasons.push("availability_gate".to_string());
        }
        if decision.unsupported_reason.is_some() {
            reasons.push("unsupported".to_string());
        }
        if !decision.warnings.is_empty() {
            reasons.extend(decision.warnings.clone());
        }
        if !reasons.is_empty() {
            out.push((surface.id.clone(), reasons));
        }
    }
    out
}

pub fn detect_claude_config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = home_dir() {
        candidates.push(home.join(".claude/settings.json"));
        candidates.push(home.join(".claude/config.json"));
    }
    candidates.push(cwd.join(".claude/settings.json"));
    candidates
}

fn find_binary_in_path(binary: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|path| path.join(binary))
        .find(|path| path.exists())
        .map(|path| path.display().to_string())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

async fn version_from_binary(binary: &str) -> Result<String> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .await
        .with_context(|| format!("failed to run {binary} --version"))?;

    if !output.status.success() {
        anyhow::bail!("{binary} --version exited with {}", output.status);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn resolve_port(cli_port: Option<u16>) -> u16 {
    if let Some(port) = cli_port {
        return port;
    }
    if let Ok(raw) = std::env::var("PROXY_PORT") {
        match raw.parse::<u16>() {
            Ok(port) => return port,
            Err(_) => log::warn!("Invalid PROXY_PORT='{raw}', fallback to 8080"),
        }
    }
    8080
}

pub fn resolve_auth_path(cli_auth_path: Option<String>) -> String {
    if let Some(path) = cli_auth_path {
        return path;
    }
    if let Ok(path) = std::env::var("PROXY_AUTH_PATH") {
        if !path.trim().is_empty() {
            return path;
        }
    }
    "~/.codex/auth.json".to_string()
}

pub fn resolve_skills_registry_path(cli_registry_path: Option<String>) -> Option<String> {
    if let Some(path) = cli_registry_path.filter(|v| !v.trim().is_empty()) {
        return Some(path);
    }
    std::env::var("PROXY_SKILLS_REGISTRY_PATH")
        .ok()
        .filter(|v| !v.trim().is_empty())
}
