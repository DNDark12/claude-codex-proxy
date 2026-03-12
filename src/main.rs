use anyhow::Result;
use clap::Parser;
use env_logger::Env;

mod domain;
mod proxy;
mod routes;
mod translation;

use crate::proxy::codex_client::CodexClient;
use crate::routes::build_routes;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Path to Codex auth.json file
    #[arg(long, default_value = "~/.codex/auth.json")]
    auth_path: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    let client = CodexClient::from_auth_path(&args.auth_path).await?;
    let routes = build_routes(client);

    log::info!("Proxy listening on http://0.0.0.0:{}", args.port);

    warp::serve(routes).run(([0, 0, 0, 0], args.port)).await;
    Ok(())
}
