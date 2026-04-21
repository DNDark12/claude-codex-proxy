use anyhow::Result;
use clap::Parser;
use env_logger::Env;

use claude_codex_proxy::app_server;
use claude_codex_proxy::app_server::AppServerClient;
use claude_codex_proxy::cli;
use claude_codex_proxy::cli::{CliCommand, RuntimeArgs};
use claude_codex_proxy::jobs::{JobExecutor, JobRegistry};
use claude_codex_proxy::proxy::codex_client::CodexClient;
use claude_codex_proxy::routes::{build_routes, RouteBuildOptions};
use claude_codex_proxy::skills::load_skill_registry;
use claude_codex_proxy::state::StateStore;
use claude_codex_proxy::surfaces::{CompatibilityMatrix, OperationMode, SurfaceRegistry};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<CliCommand>,

    #[command(flatten)]
    runtime: RuntimeArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    match args.command {
        Some(CliCommand::Setup(setup_args)) => {
            let report = cli::setup::run_setup(&setup_args).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Some(CliCommand::Doctor(doctor_args)) => {
            let report = cli::doctor::run_doctor(&doctor_args).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Some(CliCommand::Env(env_args)) => {
            println!("{}", cli::env::render_env(&env_args).await?);
            Ok(())
        }
        Some(CliCommand::Serve(runtime_args)) => serve(runtime_args).await,
        None => serve(args.runtime).await,
    }
}

async fn serve(runtime_args: RuntimeArgs) -> Result<()> {
    let runtime = runtime_args.resolve();
    let skill_registry = load_optional_skill_registry(runtime.skills_registry_path.as_deref());
    let surface_registry = SurfaceRegistry::new();
    let compatibility_matrix = CompatibilityMatrix::new(&surface_registry);
    let job_registry = JobRegistry::default();
    let state_store = StateStore::default();

    let app_server = initialize_app_server(&runtime).await?;
    let legacy_client = initialize_legacy_client(&runtime).await?;
    let executor = app_server.clone().map(|client| {
        JobExecutor::with_runtime(
            client,
            job_registry.clone(),
            state_store.clone(),
            runtime.operation_mode,
            runtime.api_stability,
            runtime.delegation_policy.clone(),
        )
    });

    let routes = build_routes(RouteBuildOptions {
        client: legacy_client,
        app_server,
        executor,
        skill_registry,
        surface_registry,
        compatibility_matrix,
        job_registry,
        state_store,
        operation_mode: runtime.operation_mode,
        api_stability: runtime.api_stability,
        delegation_policy: runtime.delegation_policy,
    });

    log::info!("Proxy listening on http://0.0.0.0:{}", runtime.port);
    log::info!("Operation mode: {:?}", runtime.operation_mode);
    log::info!("Using auth path: {}", runtime.auth_path);
    match &runtime.skills_registry_path {
        Some(path) => log::info!("Using skills registry path: {path}"),
        None => log::info!("Skills registry disabled"),
    }

    warp::serve(routes)
        .try_bind(([0, 0, 0, 0], runtime.port))
        .await;
    Ok(())
}

async fn initialize_app_server(runtime: &cli::RuntimeConfig) -> Result<Option<AppServerClient>> {
    if matches!(runtime.operation_mode, OperationMode::ResponsesOnly) {
        return Ok(None);
    }

    match AppServerClient::connect(app_server::AppServerConnectOptions {
        api_stability: runtime.api_stability,
        ..app_server::AppServerConnectOptions::default()
    })
    .await
    {
        Ok(client) => {
            log::info!(
                "App-server handshake complete: {}",
                client.handshake().user_agent
            );
            Ok(Some(client))
        }
        Err(error) if matches!(runtime.operation_mode, OperationMode::AutoHybrid) => {
            log::warn!("App-server unavailable, continuing in degraded mode: {error}");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

async fn initialize_legacy_client(runtime: &cli::RuntimeConfig) -> Result<Option<CodexClient>> {
    match CodexClient::from_auth_path(&runtime.auth_path).await {
        Ok(client) => Ok(Some(client)),
        Err(error)
            if matches!(
                runtime.operation_mode,
                OperationMode::AutoHybrid | OperationMode::StrictAppServer
            ) =>
        {
            log::warn!("Responses API fallback unavailable: {error}");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn load_optional_skill_registry(path: Option<&str>) -> Option<claude_codex_proxy::skills::SkillRegistry> {
    let path = path?;
    match load_skill_registry(path) {
        Ok(registry) => {
            log::info!(
                "Loaded skills registry version={} entries={}",
                registry.version,
                registry.len()
            );
            Some(registry)
        }
        Err(err) => {
            log::warn!("Failed to load skills registry at {path}: {err}");
            None
        }
    }
}
