use anyhow::Result;
use clap::Parser;
use env_logger::Env;

mod domain;
mod proxy;
mod routes;
mod skills;
mod translation;

use crate::proxy::codex_client::CodexClient;
use crate::routes::build_routes;
use crate::skills::load_skill_registry;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port to listen on
    #[arg(short, long)]
    port: Option<u16>,

    /// Path to Codex auth.json file
    #[arg(long)]
    auth_path: Option<String>,

    /// Path to a generated skill registry json file
    #[arg(long)]
    skills_registry_path: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let port = resolve_port(args.port);
    let auth_path = resolve_auth_path(args.auth_path);
    let skills_registry_path = resolve_skills_registry_path(args.skills_registry_path);
    let skill_registry = load_optional_skill_registry(skills_registry_path.as_deref());

    let client = CodexClient::from_auth_path(&auth_path).await?;
    let routes = build_routes(client, skill_registry);

    log::info!("Proxy listening on http://0.0.0.0:{port}");
    log::info!("Using auth path: {auth_path}");
    match &skills_registry_path {
        Some(path) => log::info!("Using skills registry path: {path}"),
        None => log::info!("Skills registry disabled"),
    }

    warp::serve(routes).run(([0, 0, 0, 0], port)).await;
    Ok(())
}

fn resolve_port(cli_port: Option<u16>) -> u16 {
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

fn resolve_auth_path(cli_auth_path: Option<String>) -> String {
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

fn resolve_skills_registry_path(cli_registry_path: Option<String>) -> Option<String> {
    if let Some(path) = cli_registry_path.filter(|v| !v.trim().is_empty()) {
        return Some(path);
    }

    std::env::var("PROXY_SKILLS_REGISTRY_PATH")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

fn load_optional_skill_registry(path: Option<&str>) -> Option<crate::skills::SkillRegistry> {
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
